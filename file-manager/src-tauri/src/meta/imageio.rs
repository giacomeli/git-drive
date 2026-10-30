//! HEIC and HEIF metadata changed on macOS through ImageIO: the picture is
//! copied as it is (`CGImageDestinationCopyImageSource`, no re-encoding)
//! with its metadata edited as a tree of tags (the EXIF included, which
//! ImageIO maps to `exif:` and `tiff:` paths), written beside the original
//! and renamed over it.

use std::path::Path;

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, CFURL};
use objc2_image_io::{
    kCGImageDestinationMergeMetadata, kCGImageDestinationMetadata, kCGImageMetadataShouldExcludeGPS, kCGImageMetadataShouldExcludeXMP, CGImageDestination, CGImageSource,
    CGMutableImageMetadata,
};

use super::{exif_date_to_iso, iso_to_exif_date, MetaEdit};

fn cfstr(s: &str) -> CFRetained<CFString> {
    CFString::from_str(s)
}

/// Sets (or, for an empty value, removes) the tag at `path`.
fn put(meta: &CGMutableImageMetadata, path: &str, value: Option<&CFType>) {
    let p = cfstr(path);
    // SAFETY: a mutable metadata tree and a path; the value is a CF type ImageIO takes.
    unsafe {
        meta.remove_tag_with_path(None, &p);
        if let Some(v) = value {
            meta.set_value_with_path(None, &p, v);
        }
    }
}

fn text(meta: &CGMutableImageMetadata, path: &str, value: &str) {
    let v = value.trim();
    let s = cfstr(v);
    put(meta, path, (!v.is_empty()).then_some(&**s as &CFType));
}

fn list(meta: &CGMutableImageMetadata, path: &str, values: &[String]) {
    let items: Vec<CFRetained<CFString>> = values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()).map(cfstr).collect();
    let arr = CFArray::from_retained_objects(&items);
    put(meta, path, (!items.is_empty()).then_some(&**arr as &CFType));
}

/// The keywords there now (`dc:subject`), for an edit that adds to them.
fn keywords_of(path: &Path) -> Vec<String> {
    std::fs::read(path).ok().and_then(|b| super::xmp::find_packet(&b)).map(|p| super::xmp::read(&p).subject).unwrap_or_default()
}

pub fn write(path: &Path, edit: &MetaEdit) -> Result<(), String> {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let fail = |why: String| t!("meta.change_failed", name = name, reason = why);
    let url = CFURL::from_file_path(path).ok_or_else(|| fail(t!("meta.why.path")))?;
    let tmp = super::temp_beside(path)?;
    let tmp_url = CFURL::from_file_path(&tmp).ok_or_else(|| fail(t!("meta.why.path")))?;
    // SAFETY: CoreFoundation and ImageIO objects created and used on this thread only.
    let copied = unsafe {
        let src = CGImageSource::with_url(&url, None).ok_or_else(|| fail(t!("meta.why.not_readable")))?;
        let uti = src.r#type().ok_or_else(|| fail(t!("meta.why.unknown_type")))?;
        let meta = if edit.strip_all {
            let m = CGMutableImageMetadata::new();
            // The orientation stays, or the picture would show turned.
            let n = std::fs::File::open(path)
                .ok()
                .and_then(|f| exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok())
                .and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)));
            if let Some(n) = n {
                text(&m, "tiff:Orientation", &n.to_string());
            }
            m
        } else {
            let current = src.metadata_at_index(0, None);
            let m = match current {
                Some(c) => CGMutableImageMetadata::new_copy(&c).ok_or_else(|| fail(t!("meta.why.metadata")))?,
                None => CGMutableImageMetadata::new(),
            };
            if let Some(v) = &edit.title {
                text(&m, "dc:title", v);
            }
            if let Some(v) = &edit.description {
                text(&m, "dc:description", v);
            }
            if let Some(v) = &edit.copyright {
                text(&m, "dc:rights", v);
            }
            if let Some(v) = &edit.creator {
                list(&m, "dc:creator", std::slice::from_ref(v));
            }
            let mut keywords = edit.keywords.clone();
            if !edit.add_keywords.is_empty() {
                let mut all = keywords.take().unwrap_or_else(|| keywords_of(path));
                for k in &edit.add_keywords {
                    if !all.iter().any(|a| a.eq_ignore_ascii_case(k)) {
                        all.push(k.clone());
                    }
                }
                keywords = Some(all);
            }
            if let Some(k) = &keywords {
                list(&m, "dc:subject", k);
            }
            if let Some(r) = edit.rating {
                if !(0..=5).contains(&r) {
                    return Err(t!("meta.rating_range"));
                }
                let stars = if r == 0 { String::new() } else { r.to_string() };
                text(&m, "xmp:Rating", &stars);
            }
            if let Some(t) = &edit.taken {
                let exif_form = iso_to_exif_date(t).ok_or_else(|| t!("meta.not_a_date", value = t))?;
                let iso = exif_date_to_iso(&exif_form).expect("round trip");
                text(&m, "exif:DateTimeOriginal", &iso);
                text(&m, "exif:DateTimeDigitized", &iso);
                text(&m, "photoshop:DateCreated", &iso);
            }
            m
        };
        let dest = CGImageDestination::with_url(&tmp_url, &uti, 1, None).ok_or_else(|| fail(t!("meta.why.no_destination")))?;
        let gps = CFBoolean::new(edit.remove_location || edit.strip_all);
        // The XMP packet itself goes too when stripping (an empty tree alone leaves it).
        let xmp = CFBoolean::new(edit.strip_all);
        let keys: [&CFString; 4] = [kCGImageDestinationMetadata, kCGImageDestinationMergeMetadata, kCGImageMetadataShouldExcludeGPS, kCGImageMetadataShouldExcludeXMP];
        let values: [&CFType; 4] = [&meta, CFBoolean::new(false), gps, xmp];
        let options = CFDictionary::from_slices(&keys, &values);
        let mut err = std::ptr::null_mut();
        dest.copy_image_source(&src, Some(options.as_opaque()), &mut err)
    };
    if !copied || !tmp.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail(t!("meta.why.refused")));
    }
    // ImageIO copies a HEIC's XMP item as it is, whatever the options say:
    // when stripping, it is emptied in place.
    if edit.strip_all {
        let emptied = std::fs::read(&tmp).map(|mut b| super::xmp::blank_in_place(&mut b).then_some(b));
        if let Ok(Some(b)) = emptied {
            if let Err(e) = std::fs::write(&tmp, b) {
                let _ = std::fs::remove_file(&tmp);
                return Err(t!("meta.save_failed", name = name, error = crate::i18n::io_text(&e)));
            }
        }
    }
    super::finish(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A HEIC made from the sample JPEG by the system's `sips`.
    fn sample_heic(dir: &Path) -> std::path::PathBuf {
        let jpg = dir.join("src.jpg");
        std::fs::write(&jpg, super::super::jpeg::tests::sample_jpeg(Some(&super::super::tiff::tests::sample_exif(false)))).unwrap();
        let heic = dir.join("IMG_0001.HEIC");
        assert!(Command::new("sips").args(["-s", "format", "heic"]).arg(&jpg).arg("--out").arg(&heic).output().unwrap().status.success());
        heic
    }

    #[test]
    fn a_heic_takes_an_edit_and_keeps_its_picture() {
        let dir = tempfile::tempdir().unwrap();
        let heic = sample_heic(dir.path());
        let before = super::super::read(&heic).unwrap().unwrap();
        assert_eq!(before.format, "HEIC");
        assert!(before.gps.is_some(), "sips keeps the EXIF");
        let edit = MetaEdit {
            title: Some("Fim de tarde".into()),
            keywords: Some(vec!["praia".into(), "verão".into()]),
            creator: Some("Ana".into()),
            rating: Some(4),
            taken: Some("2025-12-31T23:59".into()),
            remove_location: true,
            ..MetaEdit::default()
        };
        super::super::write(&heic, &edit).unwrap();
        let m = super::super::read(&heic).unwrap().unwrap();
        assert_eq!(m.title.as_deref(), Some("Fim de tarde"));
        assert_eq!(m.keywords, ["praia", "verão"]);
        assert_eq!(m.creator.as_deref(), Some("Ana"));
        assert_eq!(m.rating, Some(4));
        assert_eq!(m.taken.as_deref(), Some("2025-12-31T23:59:00"));
        assert!(m.gps.is_none(), "location gone");
        assert!(m.camera.iter().any(|f| f.value == "EOS R6"), "the camera stays");
        assert_eq!((m.width, m.height), (before.width, before.height));
        super::super::write(&heic, &MetaEdit { strip_all: true, ..MetaEdit::default() }).unwrap();
        let bare = super::super::read(&heic).unwrap().unwrap();
        assert!(bare.camera.is_empty() && bare.title.is_none(), "{bare:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "no temporary file left");
    }
}
