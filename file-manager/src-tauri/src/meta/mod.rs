//! Image metadata for the Properties dialog: read in pure Rust from any
//! image (EXIF through `kamadak-exif`: JPEG, HEIF, PNG, WebP, TIFF and the
//! raw formats built on it; XMP from the file or its sidecar), and changed
//! where the bytes can be rewritten without re-encoding the picture:
//!
//! - JPEG and PNG, here: the description goes to the XMP (merged: what the
//!   app does not manage stays), the date and the location are changed in
//!   the EXIF in place (`tiff.rs`: maker notes keep their offsets);
//! - HEIC and HEIF on macOS, through ImageIO (`imageio.rs`);
//! - anything else (raw files, TIFF, WebP; HEIC on Linux): the description
//!   goes to an XMP sidecar, `name.ext.xmp`, as darktable writes it.
//!
//! A changed file is written beside the original, checked (it still reads
//! as the same picture) and renamed over it, with its permissions and
//! extended attributes.

pub mod jpeg;
pub mod png;
pub mod tiff;
pub mod xmp;
#[cfg(target_os = "macos")]
mod imageio;

use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use exif::{In, Tag, Value};
use serde::{Deserialize, Serialize};

use crate::tasks::{Task, CANCELLED};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Jpeg,
    Png,
    Heif,
    Webp,
    Tiff,
    Gif,
    Bmp,
}

/// Extensions of raw files (most are TIFF inside; CR3 is HEIF-like).
const RAW: [&str; 14] = ["dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef", "srw", "3fr"];

fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// What kind of image the first bytes say this is.
pub fn container(head: &[u8]) -> Option<Container> {
    match head {
        [0xff, 0xd8, 0xff, ..] => Some(Container::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(Container::Png),
        [b'G', b'I', b'F', b'8', ..] => Some(Container::Gif),
        [b'B', b'M', ..] => Some(Container::Bmp),
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Some(Container::Tiff),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(Container::Webp),
        [_, _, _, _, b'f', b't', b'y', b'p', brand @ ..] if brand.len() >= 4 => {
            let b = &brand[..4];
            [&b"heic"[..], b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif", b"crx "].contains(&b).then_some(Container::Heif)
        }
        _ => None,
    }
}

fn format_name(c: Container, ext: &str) -> String {
    if RAW.contains(&ext) {
        return t!("meta.raw_format", ext = ext.to_ascii_uppercase());
    }
    match c {
        Container::Jpeg => "JPEG",
        Container::Png => "PNG",
        Container::Heif if ext == "avif" => "AVIF",
        Container::Heif => "HEIC",
        Container::Webp => "WebP",
        Container::Tiff => "TIFF",
        Container::Gif => "GIF",
        Container::Bmp => "BMP",
    }
    .to_string()
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Fact {
    /// In the window's language.
    pub label: String,
    pub value: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Gps {
    pub lat: f64,
    pub lon: f64,
    pub altitude: Option<f64>,
    /// Where the camera pointed, in degrees from north.
    pub direction: Option<f64>,
}

/// Where this app can write an image's description, and whether it can take
/// the location or all metadata out of it.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Editing {
    /// "embedded" (in the file), "sidecar" (an XMP file beside it) or "none".
    pub descriptive: &'static str,
    pub privacy: bool,
    /// Why some of it can't, in the dialog's words.
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ImageMeta {
    pub format: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub rating: Option<i32>,
    /// When it was taken: "YYYY-MM-DDTHH:MM:SS".
    pub taken: Option<String>,
    /// The time zone it was taken in ("+02:00"), when the camera wrote it.
    pub taken_offset: Option<String>,
    pub camera: Vec<Fact>,
    pub exposure: Vec<Fact>,
    pub image: Vec<Fact>,
    pub gps: Option<Gps>,
    /// A location is there (in the EXIF or the XMP), even one not shown.
    pub has_location: bool,
    pub editing: Editing,
    pub sidecar: Option<String>,
}

fn ascii(f: &exif::Field) -> Option<String> {
    match &f.value {
        Value::Ascii(v) => v.first().map(|s| String::from_utf8_lossy(s).trim_end_matches('\0').trim().to_string()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

fn shown(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let f = exif.get_field(tag, In::PRIMARY)?;
    ascii(f).or_else(|| {
        let s = f.display_value().with_unit(exif).to_string();
        (!s.is_empty() && s != "unknown").then_some(s)
    })
}

fn rationals(f: &exif::Field) -> Option<Vec<f64>> {
    match &f.value {
        Value::Rational(v) => Some(v.iter().map(|r| r.to_f64()).collect()),
        _ => None,
    }
}

fn gps_of(exif: &exif::Exif) -> Option<Gps> {
    let coord = |tag: Tag, refs: Tag, negative: u8| -> Option<f64> {
        let v = rationals(exif.get_field(tag, In::PRIMARY)?)?;
        let deg = v.first()? + v.get(1).unwrap_or(&0.0) / 60.0 + v.get(2).unwrap_or(&0.0) / 3600.0;
        let r = exif.get_field(refs, In::PRIMARY).and_then(ascii).unwrap_or_default();
        Some(if r.as_bytes().first() == Some(&negative) { -deg } else { deg })
    };
    let lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S')?;
    let lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W')?;
    if !lat.is_finite() || !lon.is_finite() || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    let altitude = exif.get_field(Tag::GPSAltitude, In::PRIMARY).and_then(rationals).and_then(|v| v.first().copied()).map(|a| {
        let below = exif.get_field(Tag::GPSAltitudeRef, In::PRIMARY).and_then(|f| f.value.get_uint(0)) == Some(1);
        if below {
            -a
        } else {
            a
        }
    });
    let direction = exif.get_field(Tag::GPSImgDirection, In::PRIMARY).and_then(rationals).and_then(|v| v.first().copied());
    Some(Gps { lat, lon, altitude, direction })
}

/// "2024:07:01 18:30:00" -> "2024-07-01T18:30:00".
pub(crate) fn exif_date_to_iso(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() < 19 || s.starts_with("0000") {
        return None;
    }
    let (d, t) = s.split_at(10);
    Some(format!("{}T{}", d.replace(':', "-"), t.trim()))
}

/// "2024-07-01T18:30:00" (seconds optional) -> "2024:07:01 18:30:00".
pub(crate) fn iso_to_exif_date(s: &str) -> Option<String> {
    let s = s.trim();
    let (d, t) = s.split_once('T')?;
    let t = &t[..t.len().min(8)];
    let t = if t.len() == 5 { format!("{t}:00") } else { t.to_string() };
    let ok = d.len() == 10 && t.len() == 8 && d.chars().chain(t.chars()).all(|c| c.is_ascii_digit() || c == '-' || c == ':');
    ok.then(|| format!("{} {t}", d.replace('-', ":")))
}

/// The XMP sidecar beside `path` that exists: `name.ext.xmp`, else `name.xmp`.
pub fn sidecar_of(path: &Path) -> Option<PathBuf> {
    let full = PathBuf::from(format!("{}.xmp", path.display()));
    if full.is_file() {
        return Some(full);
    }
    let stem = path.with_extension("xmp");
    (stem != path && stem.is_file()).then_some(stem)
}

fn editing_for(c: Container) -> Editing {
    match c {
        Container::Jpeg | Container::Png => Editing { descriptive: "embedded", privacy: true, note: None },
        Container::Heif if cfg!(target_os = "macos") => Editing { descriptive: "embedded", privacy: true, note: None },
        Container::Heif => Editing {
            descriptive: "sidecar",
            privacy: false,
            note: Some(t!("meta.note.heic_linux")),
        },
        Container::Gif | Container::Bmp => Editing { descriptive: "none", privacy: false, note: Some(t!("meta.note.none")) },
        _ => Editing {
            descriptive: "sidecar",
            privacy: false,
            note: Some(t!("meta.note.sidecar")),
        },
    }
}

/// The XMP packet of the file itself.
fn embedded_xmp(c: Container, bytes: &[u8]) -> Option<String> {
    match c {
        Container::Jpeg => jpeg::xmp(bytes),
        Container::Png => png::xmp(bytes),
        _ => xmp::find_packet(bytes),
    }
}

/// Reads an image's metadata; None when `path` is no image.
pub fn read(path: &Path) -> Result<Option<ImageMeta>, String> {
    let mut head = [0u8; 16];
    let n = fs::File::open(path).and_then(|mut f| f.read(&mut head)).map_err(|e| e.to_string())?;
    let Some(c) = container(&head[..n]) else { return Ok(None) };
    let ext = ext_of(path);
    let exif = fs::File::open(path).ok().and_then(|f| exif::Reader::new().read_from_container(&mut BufReader::new(f)).ok());
    // The XMP: in the file (read whole only for the formats that need a scan of it), then a sidecar.
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let editing = editing_for(c);
    let sidecar = sidecar_of(path);
    let side_packet = sidecar.as_ref().and_then(|p| fs::read_to_string(p).ok());
    let inner_packet = embedded_xmp(c, &bytes);
    let fields = match (editing.descriptive, &side_packet, &inner_packet) {
        ("sidecar", Some(p), _) => xmp::read(p),
        (_, _, Some(p)) => xmp::read(p),
        (_, Some(p), None) => xmp::read(p),
        _ => xmp::Fields::default(),
    };

    let mut m = ImageMeta {
        format: format_name(c, &ext),
        width: None,
        height: None,
        title: fields.title.clone(),
        description: fields.description.clone(),
        keywords: fields.subject.clone(),
        creator: fields.creator.first().cloned(),
        copyright: fields.rights.clone(),
        rating: fields.rating.filter(|r| *r > 0),
        taken: None,
        taken_offset: None,
        camera: Vec::new(),
        exposure: Vec::new(),
        image: Vec::new(),
        gps: None,
        has_location: fields.has_gps,
        editing,
        sidecar: sidecar.map(|p| p.display().to_string()),
    };
    if let Some(e) = &exif {
        m.description = m.description.or_else(|| shown(e, Tag::ImageDescription));
        m.creator = m.creator.or_else(|| shown(e, Tag::Artist));
        m.copyright = m.copyright.or_else(|| shown(e, Tag::Copyright));
        m.taken = e.get_field(Tag::DateTimeOriginal, In::PRIMARY).and_then(ascii).and_then(|d| exif_date_to_iso(&d));
        m.taken_offset = shown(e, Tag::OffsetTimeOriginal);
        let facts = |list: &[(String, Tag)]| -> Vec<Fact> { list.iter().filter_map(|(label, tag)| shown(e, *tag).map(|value| Fact { label: label.clone(), value })).collect() };
        m.camera = facts(&[
            (t!("meta.fact.maker"), Tag::Make),
            (t!("meta.fact.model"), Tag::Model),
            (t!("meta.fact.lens"), Tag::LensModel),
            (t!("meta.fact.lens_maker"), Tag::LensMake),
            (t!("meta.fact.software"), Tag::Software),
        ]);
        m.exposure = facts(&[
            (t!("meta.fact.shutter"), Tag::ExposureTime),
            (t!("meta.fact.aperture"), Tag::FNumber),
            (t!("meta.fact.iso"), Tag::PhotographicSensitivity),
            (t!("meta.fact.focal_length"), Tag::FocalLength),
            (t!("meta.fact.in_35mm"), Tag::FocalLengthIn35mmFilm),
            (t!("meta.fact.compensation"), Tag::ExposureBiasValue),
            (t!("meta.fact.program"), Tag::ExposureProgram),
            (t!("meta.fact.metering"), Tag::MeteringMode),
            (t!("meta.fact.flash"), Tag::Flash),
            (t!("meta.fact.white_balance"), Tag::WhiteBalance),
        ]);
        m.image = facts(&[
            (t!("meta.fact.orientation"), Tag::Orientation),
            (t!("meta.fact.color_space"), Tag::ColorSpace),
            (t!("meta.fact.resolution"), Tag::XResolution),
        ]);
        m.width = e.get_field(Tag::PixelXDimension, In::PRIMARY).and_then(|f| f.value.get_uint(0));
        m.height = e.get_field(Tag::PixelYDimension, In::PRIMARY).and_then(|f| f.value.get_uint(0));
        m.gps = gps_of(e);
        m.has_location |= m.gps.is_some() || e.fields().any(|f| f.ifd_num == In::PRIMARY && f.tag.context() == exif::Context::Gps);
    }
    if m.taken.is_none() {
        m.taken = fields.date_created.as_deref().map(|d| d.chars().take(19).collect());
    }
    if let (None, Ok((w, h))) = (m.width, image::image_dimensions(path)) {
        m.width = Some(w);
        m.height = Some(h);
    }
    Ok(Some(m))
}

/// What the dialog changes: each field present is set ("" clears it), the
/// keywords are replaced (`keywords`) or added to (`add_keywords`, for
/// several images), a rating of 0 clears it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MetaEdit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub keywords: Option<Vec<String>>,
    #[serde(default)]
    pub add_keywords: Vec<String>,
    pub rating: Option<i32>,
    /// "YYYY-MM-DDTHH:MM[:SS]".
    pub taken: Option<String>,
    #[serde(default)]
    pub remove_location: bool,
    /// Everything out but the orientation and the color profile.
    #[serde(default)]
    pub strip_all: bool,
}

impl MetaEdit {
    pub(crate) fn descriptive(&self) -> bool {
        self.title.is_some()
            || self.description.is_some()
            || self.creator.is_some()
            || self.copyright.is_some()
            || self.keywords.is_some()
            || !self.add_keywords.is_empty()
            || self.rating.is_some()
            || self.taken.is_some()
    }

    /// The XMP side of the edit, given the keywords there now.
    fn xmp(&self, current: &xmp::Fields) -> Result<xmp::Edit, String> {
        use xmp::{Value as V, DC, EXIF, PHOTOSHOP, XMP};
        let text = |v: &String| (!v.trim().is_empty()).then(|| v.trim().to_string());
        let mut props = Vec::new();
        if let Some(v) = &self.title {
            props.push((DC, "title", text(v).map(V::Alt)));
        }
        if let Some(v) = &self.description {
            props.push((DC, "description", text(v).map(V::Alt)));
        }
        if let Some(v) = &self.copyright {
            props.push((DC, "rights", text(v).map(V::Alt)));
        }
        if let Some(v) = &self.creator {
            props.push((DC, "creator", text(v).map(|t| V::Seq(vec![t]))));
        }
        let mut keywords = self.keywords.clone();
        if !self.add_keywords.is_empty() {
            let mut all = keywords.take().unwrap_or_else(|| current.subject.clone());
            for k in &self.add_keywords {
                if !all.iter().any(|a| a.eq_ignore_ascii_case(k)) {
                    all.push(k.clone());
                }
            }
            keywords = Some(all);
        }
        if let Some(list) = keywords {
            let list: Vec<String> = list.iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
            props.push((DC, "subject", (!list.is_empty()).then_some(V::Bag(list))));
        }
        if let Some(r) = self.rating {
            if !(0..=5).contains(&r) {
                return Err(t!("meta.rating_range"));
            }
            props.push((XMP, "Rating", (r > 0).then(|| V::Simple(r.to_string()))));
        }
        if let Some(t) = &self.taken {
            let exif_form = iso_to_exif_date(t).ok_or_else(|| t!("meta.not_a_date", value = t))?;
            let iso = exif_date_to_iso(&exif_form).expect("round trip");
            props.push((PHOTOSHOP, "DateCreated", Some(V::Simple(iso.clone()))));
            props.push((EXIF, "DateTimeOriginal", Some(V::Simple(iso))));
        }
        let remove_starting = if self.remove_location { vec![(EXIF, "GPS")] } else { Vec::new() };
        Ok(xmp::Edit { props, remove_starting })
    }

    /// The EXIF side, in place (same size).
    fn patch_exif(&self, block: &mut [u8]) {
        let Some(mut t) = tiff::Tiff::new(block) else { return };
        for (field, tag) in [(&self.description, tiff::IMAGE_DESCRIPTION), (&self.creator, tiff::ARTIST), (&self.copyright, tiff::COPYRIGHT)] {
            if let Some(v) = field {
                t.set_text(tag, v.trim());
            }
        }
        if let Some(d) = self.taken.as_deref().and_then(iso_to_exif_date) {
            t.set_text(tiff::DATE_TIME_ORIGINAL, &d);
            t.set_text(tiff::DATE_TIME_DIGITIZED, &d);
        }
        if self.remove_location {
            t.remove_gps();
        }
    }
}

fn orientation_in(c: Container, bytes: &[u8]) -> Option<u16> {
    let range = match c {
        Container::Jpeg => jpeg::exif_range(bytes)?,
        _ => {
            let e = png::exif(bytes)?;
            let start = e.as_ptr() as usize - bytes.as_ptr() as usize;
            start..start + e.len()
        }
    };
    let mut block = bytes[range].to_vec();
    tiff::Tiff::new(&mut block)?.orientation()
}

/// The new bytes of a JPEG or PNG after `edit`.
fn rewritten(c: Container, bytes: &[u8], edit: &MetaEdit) -> Result<Vec<u8>, String> {
    if edit.strip_all {
        let o = orientation_in(c, bytes);
        return match c {
            Container::Jpeg => jpeg::stripped(bytes, o),
            _ => png::stripped(bytes, o),
        };
    }
    let packet = embedded_xmp(c, bytes);
    let current = packet.as_deref().map(xmp::read).unwrap_or_default();
    let changes = edit.xmp(&current)?;
    let mut out = if changes.is_empty() {
        bytes.to_vec()
    } else {
        let merged = xmp::merge(packet.as_deref(), &changes);
        match c {
            Container::Jpeg => jpeg::with_xmp(bytes, &merged)?,
            _ => png::with_xmp(bytes, &merged)?,
        }
    };
    match c {
        Container::Jpeg => {
            if let Some(r) = jpeg::exif_range(&out) {
                edit.patch_exif(&mut out[r]);
            }
        }
        _ => {
            if png::exif(&out).is_some() {
                out = png::with_exif_changed(&out, |block| edit.patch_exif(block))?;
            }
        }
    }
    Ok(out)
}

/// The picture's size as its header says (no decoding), to check a rewrite.
fn header_size(c: Container, bytes: &[u8]) -> Option<(u32, u32)> {
    let format = match c {
        Container::Jpeg => image::ImageFormat::Jpeg,
        _ => image::ImageFormat::Png,
    };
    image::ImageReader::with_format(std::io::Cursor::new(bytes), format).into_dimensions().ok()
}

/// A free name beside `path` for its new version.
pub(crate) fn temp_beside(path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or_else(|| t!("meta.no_file_name"))?.to_string_lossy().into_owned();
    let dir = path.parent().ok_or_else(|| t!("meta.no_folder"))?;
    Ok(dir.join(format!(".{name}.meta-{}", std::process::id())))
}

/// Puts the new version `tmp` in place of `path`, with its permissions and
/// extended attributes (Finder tags, comments); `tmp` is removed on failure.
pub(crate) fn finish(tmp: &Path, path: &Path) -> Result<(), String> {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let result = (|| -> std::io::Result<()> {
        fs::set_permissions(tmp, fs::metadata(path)?.permissions())?;
        if let Ok(names) = xattr::list(path) {
            for n in names {
                if let Ok(Some(v)) = xattr::get(path, &n) {
                    let _ = xattr::set(tmp, &n, &v);
                }
            }
        }
        fs::rename(tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result.map_err(|e| t!("meta.save_failed", name = name, error = crate::i18n::io_text(&e)))
}

/// Writes `bytes` beside `path` and renames them over it.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = temp_beside(path)?;
    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(t!("meta.save_failed", name = path.display(), error = crate::i18n::io_text(&e)));
    }
    finish(&tmp, path)
}

/// Applies `edit` to one image.
pub fn write(path: &Path, edit: &MetaEdit) -> Result<(), String> {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let bytes = fs::read(path).map_err(|e| t!("meta.read_failed", name = name, error = crate::i18n::io_text(&e)))?;
    let c = container(&bytes).ok_or_else(|| t!("meta.not_an_image", name = name))?;
    let editing = editing_for(c);
    if (edit.remove_location || edit.strip_all) && !editing.privacy {
        return Err(t!("meta.cant_remove_here", name = name, format = format_name(c, &ext_of(path))));
    }
    match (c, editing.descriptive) {
        (Container::Jpeg | Container::Png, _) => {
            let out = rewritten(c, &bytes, edit)?;
            if out == bytes {
                return Ok(());
            }
            // The same picture, as its header says, or nothing is written.
            if header_size(c, &out).is_none() || header_size(c, &out) != header_size(c, &bytes) {
                return Err(t!("meta.not_safe", name = name));
            }
            replace(path, &out)
        }
        #[cfg(target_os = "macos")]
        (Container::Heif, _) => imageio::write(path, edit),
        (_, "sidecar") => {
            if !edit.descriptive() {
                return Ok(());
            }
            let side = sidecar_of(path).unwrap_or_else(|| PathBuf::from(format!("{}.xmp", path.display())));
            let packet = fs::read_to_string(&side).ok();
            let current = packet.as_deref().map(xmp::read).unwrap_or_default();
            let merged = xmp::merge(packet.as_deref(), &edit.xmp(&current)?);
            if side.exists() {
                replace(&side, merged.as_bytes())
            } else {
                fs::write(&side, merged).map_err(|e| t!("meta.write_failed", path = side.display(), error = crate::i18n::io_text(&e)))
            }
        }
        _ => Err(t!("meta.no_metadata", name = name)),
    }
}

/// Applies `edit` to every image of `paths` (the others are skipped),
/// counting each on `task`; one that fails does not stop the rest. Returns
/// how many changed, or what went wrong.
pub fn write_many(paths: &[PathBuf], edit: &MetaEdit, task: &Task) -> Result<usize, String> {
    task.set_totals(0, 0, paths.len() as u64);
    let mut done = 0;
    let mut failed: Vec<String> = Vec::new();
    for path in paths {
        if task.is_cancelled() {
            return Err(CANCELLED.into());
        }
        task.on(path);
        let mut head = [0u8; 16];
        let is_image = fs::File::open(path).and_then(|mut f| f.read(&mut head)).ok().and_then(|n| container(&head[..n])).is_some();
        if is_image {
            match write(path, edit) {
                Ok(()) => done += 1,
                Err(e) => failed.push(e),
            }
        }
        task.item_done();
    }
    match failed.len() {
        0 => Ok(done),
        1 => Err(failed.remove(0)),
        n => Err(t!("meta.many_failed", count = n, first = failed[0])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(dir: &Path, name: &str, png: bool) -> PathBuf {
        let block = tiff::tests::sample_exif(png);
        let bytes = if png { png::tests::sample_png(Some(&block)) } else { jpeg::tests::sample_jpeg(Some(&block)) };
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_photo_reads_in_sections() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path(), "IMG_0001.JPG", false);
        let m = read(&p).unwrap().unwrap();
        assert_eq!(m.format, "JPEG");
        assert_eq!((m.width, m.height), (Some(16), Some(8)), "from the header when the EXIF has no size");
        assert_eq!(m.creator.as_deref(), Some("Juliano Giacomeli"), "from the EXIF when the XMP has none");
        assert_eq!(m.taken.as_deref(), Some("2024-07-01T18:30:00"));
        assert!(m.camera.contains(&Fact { label: "Model".into(), value: "EOS R6".into() }));
        assert!(m.exposure.iter().any(|f| f.label == "Aperture" && f.value.contains("2.8")));
        let g = m.gps.unwrap();
        assert!((g.lat - -25.441_666).abs() < 1e-4 && (g.lon - -49.266_666).abs() < 1e-4, "{g:?}");
        assert!(m.has_location);
        assert_eq!(m.editing.descriptive, "embedded");
        assert!(read(&dir.path().join("x.txt")).is_err() || read(&dir.path().join("x.txt")).unwrap().is_none());
        fs::write(dir.path().join("notes.txt"), "plain").unwrap();
        assert!(read(&dir.path().join("notes.txt")).unwrap().is_none());
    }

    #[test]
    fn an_edit_round_trips_in_jpeg_and_png() {
        for png in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let p = photo(dir.path(), if png { "a.png" } else { "a.jpg" }, png);
            let edit = MetaEdit {
                title: Some("Fim de tarde".into()),
                description: Some("Na praia".into()),
                creator: Some("Ana".into()),
                copyright: Some("© 2026 Ana".into()),
                keywords: Some(vec!["praia".into(), "verão".into()]),
                rating: Some(4),
                taken: Some("2025-12-31T23:59".into()),
                ..MetaEdit::default()
            };
            write(&p, &edit).unwrap();
            let m = read(&p).unwrap().unwrap();
            assert_eq!(m.title.as_deref(), Some("Fim de tarde"));
            assert_eq!(m.description.as_deref(), Some("Na praia"));
            assert_eq!(m.creator.as_deref(), Some("Ana"), "the XMP, and the EXIF field rewritten in place");
            assert_eq!(m.copyright.as_deref(), Some("© 2026 Ana"));
            assert_eq!(m.keywords, ["praia", "verão"]);
            assert_eq!(m.rating, Some(4));
            assert_eq!(m.taken.as_deref(), Some("2025-12-31T23:59:00"), "the EXIF date, changed in place");
            assert!(m.camera.iter().any(|f| f.value == "EOS R6"), "the rest of the EXIF stays");
            // Several images: keywords added, not replaced; a rating cleared.
            write_many(std::slice::from_ref(&p), &MetaEdit { add_keywords: vec!["Praia".into(), "família".into()], rating: Some(0), ..MetaEdit::default() }, &Task::default()).unwrap();
            let m = read(&p).unwrap().unwrap();
            assert_eq!(m.keywords, ["praia", "verão", "família"]);
            assert_eq!(m.rating, None);
            // Privacy.
            write(&p, &MetaEdit { remove_location: true, ..MetaEdit::default() }).unwrap();
            let m = read(&p).unwrap().unwrap();
            assert!(m.gps.is_none() && !m.has_location, "png={png}");
            assert_eq!(m.title.as_deref(), Some("Fim de tarde"));
            write(&p, &MetaEdit { strip_all: true, ..MetaEdit::default() }).unwrap();
            let m = read(&p).unwrap().unwrap();
            assert_eq!((m.title, m.creator, m.taken), (None, None, None));
            assert!(m.camera.is_empty());
            assert!(m.image.iter().any(|f| f.label == "Orientation"), "the orientation stays");
            assert!(image::open(&p).is_ok(), "still a picture");
        }
    }

    #[test]
    fn other_formats_get_a_sidecar_and_refuse_privacy_changes() {
        let dir = tempfile::tempdir().unwrap();
        // A TIFF-based raw file: EXIF read, the description in name.ext.xmp.
        let raw = dir.path().join("IMG_1.DNG");
        fs::write(&raw, tiff::tests::sample_exif(true)).unwrap();
        let m = read(&raw).unwrap().unwrap();
        assert_eq!((m.format.as_str(), m.editing.descriptive, m.editing.privacy), ("DNG raw", "sidecar", false));
        write(&raw, &MetaEdit { title: Some("Raw".into()), ..MetaEdit::default() }).unwrap();
        assert!(dir.path().join("IMG_1.DNG.xmp").is_file());
        assert_eq!(read(&raw).unwrap().unwrap().title.as_deref(), Some("Raw"));
        assert!(write(&raw, &MetaEdit { remove_location: true, ..MetaEdit::default() }).is_err());
        assert!(MetaEdit { rating: Some(9), ..MetaEdit::default() }.xmp(&xmp::Fields::default()).is_err());
        assert!(MetaEdit { taken: Some("yesterday".into()), ..MetaEdit::default() }.xmp(&xmp::Fields::default()).is_err());
    }

    /// ExifTool, where it is installed, finds nothing wrong in what was written.
    #[test]
    fn exiftool_finds_no_fault_in_a_rewrite() {
        let Ok(probe) = std::process::Command::new("exiftool").arg("-ver").output() else { return };
        if !probe.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for png in [false, true] {
            let p = photo(dir.path(), if png { "v.png" } else { "v.jpg" }, png);
            let edit = MetaEdit {
                title: Some("Título".into()),
                keywords: Some(vec!["a".into(), "b".into()]),
                creator: Some("Ana".into()),
                rating: Some(3),
                taken: Some("2025-01-02T03:04:05".into()),
                remove_location: true,
                ..MetaEdit::default()
            };
            write(&p, &edit).unwrap();
            let out = std::process::Command::new("exiftool").args(["-validate", "-warning", "-error", "-a", "-G1", "-s"]).arg(&p).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(!text.contains("[ExifTool]     Error"), "{text}");
            let tags = std::process::Command::new("exiftool").args(["-s", "-s", "-s", "-XMP-dc:Title", "-XMP-dc:Subject", "-XMP-xmp:Rating", "-ExifIFD:DateTimeOriginal", "-GPSLatitude"]).arg(&p).output().unwrap();
            let tags = String::from_utf8_lossy(&tags.stdout);
            assert_eq!(tags.lines().collect::<Vec<_>>(), ["Título", "a, b", "3", "2025:01:02 03:04:05"], "png={png}");
        }
    }

    #[test]
    fn a_rewrite_keeps_permissions_and_extended_attributes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path(), "b.jpg", false);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        let tagged = xattr::set(&p, "user.test.tag", b"red").is_ok();
        write(&p, &MetaEdit { title: Some("x".into()), ..MetaEdit::default() }).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640);
        if tagged {
            assert_eq!(xattr::get(&p, "user.test.tag").unwrap().as_deref(), Some(&b"red"[..]));
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no temporary file left");
    }
}
