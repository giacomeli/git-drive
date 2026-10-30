//! A JPEG's metadata segments (APP1 EXIF, APP1 XMP, APP13 IPTC, COM),
//! read and replaced without touching the compressed picture: everything
//! from the start of the scan on is copied as it is.

const SOI: [u8; 2] = [0xff, 0xd8];
const APP0: u8 = 0xe0;
const APP1: u8 = 0xe1;
const APP13: u8 = 0xed;
const COM: u8 = 0xfe;
const SOS: u8 = 0xda;
pub const EXIF_HEADER: &[u8] = b"Exif\0\0";
pub const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const EXTENDED_XMP_HEADER: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
/// The most a segment holds after its length field.
const MAX_SEGMENT: usize = 65533;

/// One marker segment before the scan.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub marker: u8,
    /// Where it starts (its 0xFF) and ends.
    pub start: usize,
    pub end: usize,
    /// Where its data starts (after the length field).
    pub data: usize,
}

/// The segments up to the scan, and where the scan starts; None when this is no JPEG.
pub fn segments(b: &[u8]) -> Option<(Vec<Segment>, usize)> {
    if b.get(0..2)? != SOI {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 2;
    loop {
        // Fill bytes (0xFF repeated) may come before a marker.
        while b.get(at)? == &0xff && b.get(at + 1)? == &0xff {
            at += 1;
        }
        if b.get(at)? != &0xff {
            return None;
        }
        let marker = *b.get(at + 1)?;
        if marker == SOS {
            return Some((out, at));
        }
        // Markers without a length (RSTn, TEM) do not occur before the scan in practice.
        let len = u16::from_be_bytes([*b.get(at + 2)?, *b.get(at + 3)?]) as usize;
        if len < 2 {
            return None;
        }
        let end = at + 2 + len;
        if end > b.len() {
            return None;
        }
        out.push(Segment { marker, start: at, end, data: at + 4 });
        at = end;
    }
}

fn is(b: &[u8], s: &Segment, marker: u8, header: &[u8]) -> bool {
    s.marker == marker && b[s.data..s.end].starts_with(header)
}

/// Where the EXIF block (a TIFF structure) is in the file.
pub fn exif_range(b: &[u8]) -> Option<std::ops::Range<usize>> {
    let (segs, _) = segments(b)?;
    let s = segs.iter().find(|s| is(b, s, APP1, EXIF_HEADER))?;
    Some(s.data + EXIF_HEADER.len()..s.end)
}

/// The XMP packet (the standard one; an extended part is not read).
pub fn xmp(b: &[u8]) -> Option<String> {
    let (segs, _) = segments(b)?;
    let s = segs.iter().find(|s| is(b, s, APP1, XMP_HEADER))?;
    String::from_utf8(b[s.data + XMP_HEADER.len()..s.end].to_vec()).ok()
}

fn segment(marker: u8, header: &[u8], body: &[u8]) -> Vec<u8> {
    let len = 2 + header.len() + body.len();
    let mut out = vec![0xff, marker];
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(body);
    out
}

/// The file with `packet` as its XMP (the old one, extended parts included,
/// gone), placed after the JFIF and EXIF segments as writers place it.
pub fn with_xmp(b: &[u8], packet: &str) -> Result<Vec<u8>, String> {
    if XMP_HEADER.len() + packet.len() > MAX_SEGMENT {
        return Err(t!("meta.jpeg_too_long"));
    }
    let (segs, scan) = segments(b).ok_or_else(|| t!("meta.jpeg_unreadable"))?;
    let mut out = Vec::with_capacity(b.len() + packet.len() + 4);
    out.extend_from_slice(&SOI);
    let mut placed = false;
    let leading = |s: &Segment| s.marker == APP0 || is(b, s, APP1, EXIF_HEADER);
    for (i, s) in segs.iter().enumerate() {
        if is(b, s, APP1, XMP_HEADER) || is(b, s, APP1, EXTENDED_XMP_HEADER) {
            continue;
        }
        if !placed && !leading(s) {
            out.extend(segment(APP1, XMP_HEADER, packet.as_bytes()));
            placed = true;
        }
        out.extend_from_slice(&b[s.start..s.end]);
        if !placed && i + 1 == segs.len() {
            out.extend(segment(APP1, XMP_HEADER, packet.as_bytes()));
            placed = true;
        }
    }
    if !placed {
        out.extend(segment(APP1, XMP_HEADER, packet.as_bytes()));
    }
    out.extend_from_slice(&b[scan..]);
    Ok(out)
}

/// The file without EXIF, XMP, IPTC and comments; the color profile and the
/// rest stay, and so does the orientation (in an EXIF block of its own).
pub fn stripped(b: &[u8], orientation: Option<u16>) -> Result<Vec<u8>, String> {
    let (segs, scan) = segments(b).ok_or_else(|| t!("meta.jpeg_unreadable"))?;
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&SOI);
    let mut kept_orientation = false;
    for s in &segs {
        if s.marker == APP1 || s.marker == APP13 || s.marker == COM {
            continue;
        }
        if !kept_orientation && s.marker != APP0 {
            if let Some(o) = orientation.filter(|&o| o != 1) {
                out.extend(segment(APP1, EXIF_HEADER, &super::tiff::orientation_only(o)));
            }
            kept_orientation = true;
        }
        out.extend_from_slice(&b[s.start..s.end]);
    }
    if !kept_orientation {
        if let Some(o) = orientation.filter(|&o| o != 1) {
            out.extend(segment(APP1, EXIF_HEADER, &super::tiff::orientation_only(o)));
        }
    }
    out.extend_from_slice(&b[scan..]);
    Ok(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A real (tiny) JPEG, with `exif` as its EXIF block when given.
    pub fn sample_jpeg(exif: Option<&[u8]>) -> Vec<u8> {
        let img = image::RgbImage::from_fn(16, 8, |x, y| image::Rgb([x as u8 * 16, y as u8 * 32, 128]));
        let mut plain = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut plain), image::ImageFormat::Jpeg).unwrap();
        let Some(exif) = exif else { return plain };
        // Right after SOI (and APP0, when the encoder wrote one).
        let (segs, _) = segments(&plain).unwrap();
        let after = segs.iter().take_while(|s| s.marker == APP0).last().map_or(2, |s| s.end);
        let mut out = plain[..after].to_vec();
        out.extend(segment(APP1, EXIF_HEADER, exif));
        out.extend_from_slice(&b"\xff\xfe\x00\x0cA comment!"[..]);
        out.extend_from_slice(&plain[after..]);
        out
    }

    fn decodes(b: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory_with_format(b, image::ImageFormat::Jpeg).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn the_xmp_is_replaced_and_the_picture_untouched() {
        let jpg = sample_jpeg(Some(&super::super::tiff::tests::sample_exif(false)));
        assert!(xmp(&jpg).is_none());
        let one = with_xmp(&jpg, "<x:xmpmeta>first</x:xmpmeta>").unwrap();
        assert_eq!(xmp(&one).as_deref(), Some("<x:xmpmeta>first</x:xmpmeta>"));
        let two = with_xmp(&one, "<x:xmpmeta>second</x:xmpmeta>").unwrap();
        assert_eq!(xmp(&two).as_deref(), Some("<x:xmpmeta>second</x:xmpmeta>"));
        assert_eq!(two.len(), one.len() + 1, "one packet, replaced");
        assert!(exif_range(&two).is_some(), "the EXIF stays");
        assert_eq!(decodes(&two), (16, 8));
        // The XMP follows the EXIF.
        let (segs, scan) = segments(&two).unwrap();
        let pos = |h: &[u8]| segs.iter().position(|s| is(&two, s, APP1, h)).unwrap();
        assert!(pos(EXIF_HEADER) < pos(XMP_HEADER));
        assert_eq!(&two[scan..], &jpg[segments(&jpg).unwrap().1..], "the scan is the same bytes");
        assert!(with_xmp(&jpg, &"x".repeat(70_000)).is_err());
        assert!(with_xmp(b"not a jpeg", "x").is_err());
    }

    #[test]
    fn stripping_keeps_only_the_orientation() {
        let jpg = sample_jpeg(Some(&super::super::tiff::tests::sample_exif(true)));
        let with = with_xmp(&jpg, "<x:xmpmeta>about</x:xmpmeta>").unwrap();
        let bare = stripped(&with, Some(6)).unwrap();
        assert!(xmp(&bare).is_none());
        let e = exif::Reader::new().read_raw(bare[exif_range(&bare).unwrap()].to_vec()).unwrap();
        assert_eq!(e.fields().count(), 1, "only the orientation");
        assert!(!bare.windows(10).any(|w| w == b"A comment!"), "comments gone");
        assert_eq!(decodes(&bare), (16, 8));
        let upright = stripped(&with, Some(1)).unwrap();
        assert!(exif_range(&upright).is_none(), "upright: no EXIF at all");
    }
}
