//! A PNG's metadata chunks (`eXIf`, the XMP `iTXt`, text chunks), read and
//! replaced without touching the picture's data; every chunk written gets
//! its CRC anew.

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";

/// One chunk: its type, and where its data is.
#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    pub kind: [u8; 4],
    pub start: usize,
    pub data: usize,
    pub len: usize,
}

impl Chunk {
    fn end(&self) -> usize {
        self.data + self.len + 4
    }
}

pub fn chunks(b: &[u8]) -> Option<Vec<Chunk>> {
    if !b.starts_with(SIGNATURE) {
        return None;
    }
    let mut out = Vec::new();
    let mut at = SIGNATURE.len();
    while at < b.len() {
        let len = u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind: [u8; 4] = b.get(at + 4..at + 8)?.try_into().ok()?;
        let c = Chunk { kind, start: at, data: at + 8, len };
        if c.end() > b.len() {
            return None;
        }
        out.push(c);
        at = c.end();
        if &kind == b"IEND" {
            break;
        }
    }
    Some(out)
}

pub fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    out.extend_from_slice(&h.finalize().to_be_bytes());
    out
}

fn is_xmp(b: &[u8], c: &Chunk) -> bool {
    &c.kind == b"iTXt" && b[c.data..c.data + c.len].starts_with(XMP_KEYWORD) && b.get(c.data + XMP_KEYWORD.len()) == Some(&0)
}

/// The EXIF block (`eXIf`).
pub fn exif(b: &[u8]) -> Option<&[u8]> {
    let c = chunks(b)?.into_iter().find(|c| &c.kind == b"eXIf")?;
    Some(&b[c.data..c.data + c.len])
}

/// The XMP packet of an uncompressed `iTXt` chunk.
pub fn xmp(b: &[u8]) -> Option<String> {
    let c = chunks(b)?.into_iter().find(|c| is_xmp(b, c))?;
    let data = &b[c.data..c.data + c.len];
    // keyword \0, compression flag, method, language \0, translated keyword \0, text.
    let rest = &data[XMP_KEYWORD.len() + 1..];
    if rest.first() != Some(&0) {
        return None;
    }
    let rest = &rest[2..];
    let lang_end = rest.iter().position(|&c| c == 0)?;
    let rest = &rest[lang_end + 1..];
    let tr_end = rest.iter().position(|&c| c == 0)?;
    String::from_utf8(rest[tr_end + 1..].to_vec()).ok()
}

/// The file rebuilt: each chunk through `keep` (None drops it, Some gives
/// its new data), with `insert` chunks put before the first image data.
fn rebuilt(b: &[u8], keep: impl Fn(&Chunk) -> Option<Vec<u8>>, insert: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let list = chunks(b).ok_or_else(|| t!("meta.png_unreadable"))?;
    let mut out = Vec::with_capacity(b.len() + insert.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(SIGNATURE);
    let mut inserted = false;
    for c in &list {
        if !inserted && (&c.kind == b"IDAT" || &c.kind == b"IEND") {
            for i in insert {
                out.extend_from_slice(i);
            }
            inserted = true;
        }
        match keep(c) {
            Some(data) if data.as_slice() == &b[c.data..c.data + c.len] => out.extend_from_slice(&b[c.start..c.end()]),
            Some(data) => out.extend(chunk(&c.kind, &data)),
            None => {}
        }
    }
    Ok(out)
}

/// The file with `packet` as its XMP chunk (the old one gone).
pub fn with_xmp(b: &[u8], packet: &str) -> Result<Vec<u8>, String> {
    let mut data = XMP_KEYWORD.to_vec();
    data.extend_from_slice(&[0, 0, 0, 0, 0]);
    data.extend_from_slice(packet.as_bytes());
    rebuilt(b, |c| (!is_xmp(b, c)).then(|| b[c.data..c.data + c.len].to_vec()), &[chunk(b"iTXt", &data)])
}

/// The file with its `eXIf` block changed by `change` (in place: same size).
pub fn with_exif_changed(b: &[u8], change: impl Fn(&mut [u8])) -> Result<Vec<u8>, String> {
    rebuilt(
        b,
        |c| {
            let mut data = b[c.data..c.data + c.len].to_vec();
            if &c.kind == b"eXIf" {
                change(&mut data);
            }
            Some(data)
        },
        &[],
    )
}

/// The file without EXIF, XMP and text chunks; the orientation stays (in an
/// `eXIf` of its own), and so do the color chunks.
pub fn stripped(b: &[u8], orientation: Option<u16>) -> Result<Vec<u8>, String> {
    let gone = |c: &Chunk| [b"eXIf", b"iTXt", b"tEXt", b"zTXt", b"tIME"].contains(&&c.kind);
    let insert: Vec<Vec<u8>> = orientation.filter(|&o| o != 1).map(|o| chunk(b"eXIf", &super::tiff::orientation_only(o))).into_iter().collect();
    rebuilt(b, |c| (!gone(c)).then(|| b[c.data..c.data + c.len].to_vec()), &insert)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn sample_png(exif: Option<&[u8]>) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(8, 4, |x, y| image::Rgba([x as u8 * 30, y as u8 * 60, 200, 255]));
        let mut plain = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut plain), image::ImageFormat::Png).unwrap();
        let mut extra = vec![chunk(b"tEXt", b"Comment\0hello")];
        if let Some(e) = exif {
            extra.push(chunk(b"eXIf", e));
        }
        rebuilt(&plain, |c| Some(plain[c.data..c.data + c.len].to_vec()), &extra).unwrap()
    }

    fn decodes(b: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory_with_format(b, image::ImageFormat::Png).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn the_xmp_chunk_is_replaced_and_crcs_hold() {
        let png = sample_png(Some(&super::super::tiff::tests::sample_exif(false)));
        assert!(xmp(&png).is_none());
        let one = with_xmp(&png, "<x:xmpmeta>one</x:xmpmeta>").unwrap();
        let two = with_xmp(&one, "<x:xmpmeta>two</x:xmpmeta>").unwrap();
        assert_eq!(xmp(&two).as_deref(), Some("<x:xmpmeta>two</x:xmpmeta>"));
        assert_eq!(chunks(&two).unwrap().iter().filter(|c| is_xmp(&two, c)).count(), 1);
        assert_eq!(decodes(&two), (8, 4), "the image decoder checks every CRC");
        assert!(exif(&two).is_some());
    }

    #[test]
    fn exif_changes_in_place_and_stripping_keeps_the_orientation() {
        let png = sample_png(Some(&super::super::tiff::tests::sample_exif(true)));
        let moved = with_exif_changed(&png, |e| {
            super::super::tiff::Tiff::new(e).unwrap().remove_gps();
        })
        .unwrap();
        let parsed = exif::Reader::new().read_raw(exif(&moved).unwrap().to_vec()).unwrap();
        assert!(parsed.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_none());
        assert_eq!(decodes(&moved), (8, 4));
        let bare = stripped(&with_xmp(&moved, "<x:xmpmeta/>").unwrap(), Some(6)).unwrap();
        assert!(xmp(&bare).is_none());
        assert!(!bare.windows(5).any(|w| w == b"hello"), "text chunks gone");
        assert_eq!(exif::Reader::new().read_raw(exif(&bare).unwrap().to_vec()).unwrap().fields().count(), 1);
        assert_eq!(decodes(&bare), (8, 4));
    }
}
