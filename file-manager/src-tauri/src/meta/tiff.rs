//! Changes to an EXIF block (a TIFF structure) made in place: nothing moves,
//! so every offset in it stays right, maker notes included (cameras write
//! them with offsets that a rewritten block would break). A text field takes
//! a new value that fits its room (padded with NULs) or is emptied; the GPS
//! directory is emptied and its data zeroed (the location is gone from the
//! bytes, not only unlinked).

/// Tags this module touches.
pub const IMAGE_DESCRIPTION: u16 = 0x010e;
pub const ARTIST: u16 = 0x013b;
pub const COPYRIGHT: u16 = 0x8298;
pub const ORIENTATION: u16 = 0x0112;
const EXIF_IFD: u16 = 0x8769;
const GPS_IFD: u16 = 0x8825;
pub const DATE_TIME_ORIGINAL: u16 = 0x9003;
pub const DATE_TIME_DIGITIZED: u16 = 0x9004;

const ASCII: u16 = 2;

/// Bytes per value of each TIFF type (1 BYTE ... 12 DOUBLE).
fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

/// A TIFF block's byte order and its directories.
pub struct Tiff<'a> {
    b: &'a mut [u8],
    little: bool,
}

/// One directory entry: where it is, and what it says.
#[derive(Debug, Clone, Copy)]
struct Entry {
    at: usize,
    tag: u16,
    kind: u16,
    count: u32,
}

impl<'a> Tiff<'a> {
    pub fn new(b: &'a mut [u8]) -> Option<Tiff<'a>> {
        let little = match b.get(0..4)? {
            [b'I', b'I', 42, 0] => true,
            [b'M', b'M', 0, 42] => false,
            _ => return None,
        };
        Some(Tiff { b, little })
    }

    fn u16_at(&self, at: usize) -> Option<u16> {
        let s: [u8; 2] = self.b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    }

    fn put_u16(&mut self, at: usize, v: u16) {
        let bytes = if self.little { v.to_le_bytes() } else { v.to_be_bytes() };
        self.b[at..at + 2].copy_from_slice(&bytes);
    }

    fn ifd0(&self) -> Option<usize> {
        self.u32_at(4).map(|o| o as usize)
    }

    fn entries(&self, ifd: usize) -> Vec<Entry> {
        let Some(n) = self.u16_at(ifd) else { return Vec::new() };
        (0..n as usize)
            .map(|i| ifd + 2 + i * 12)
            .map_while(|at| Some(Entry { at, tag: self.u16_at(at)?, kind: self.u16_at(at + 2)?, count: self.u32_at(at + 4)? }))
            .collect()
    }

    fn find(&self, ifd: usize, tag: u16) -> Option<Entry> {
        self.entries(ifd).into_iter().find(|e| e.tag == tag)
    }

    /// Where an entry's value lives: in the entry itself (four bytes or
    /// less) or at the offset it holds.
    fn value_range(&self, e: Entry) -> Option<std::ops::Range<usize>> {
        let len = type_size(e.kind).checked_mul(e.count as usize)?;
        let start = if len <= 4 { e.at + 8 } else { self.u32_at(e.at + 8)? as usize };
        let end = start.checked_add(len)?;
        (end <= self.b.len()).then_some(start..end)
    }

    fn sub_ifd(&self, tag: u16) -> Option<usize> {
        let e = self.find(self.ifd0()?, tag)?;
        let at = self.u32_at(e.at + 8)? as usize;
        (at + 2 <= self.b.len()).then_some(at)
    }

    /// The directory a tag lives in: the first one, or the EXIF one.
    fn holder(&self, tag: u16) -> Option<usize> {
        match tag {
            DATE_TIME_ORIGINAL | DATE_TIME_DIGITIZED => self.sub_ifd(EXIF_IFD),
            _ => self.ifd0(),
        }
    }

    /// Writes `value` into an existing text field when it fits its room;
    /// else empties the field (a reader then goes to the XMP). Returns
    /// whether the field was there.
    pub fn set_text(&mut self, tag: u16, value: &str) -> bool {
        let Some(ifd) = self.holder(tag) else { return false };
        let Some(e) = self.find(ifd, tag).filter(|e| e.kind == ASCII) else { return false };
        let Some(range) = self.value_range(e) else { return false };
        let room = range.len();
        let bytes = value.as_bytes();
        let fits = bytes.len() < room;
        let field = &mut self.b[range];
        field.fill(0);
        if fits {
            field[..bytes.len()].copy_from_slice(bytes);
        }
        true
    }

    /// The text of a field, as stored (up to its first NUL).
    #[cfg(test)]
    pub fn text(&self, tag: u16) -> Option<String> {
        let ifd = self.holder(tag)?;
        let e = self.find(ifd, tag).filter(|e| e.kind == ASCII)?;
        let r = self.value_range(e)?;
        let raw = &self.b[r];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        Some(String::from_utf8_lossy(&raw[..end]).into_owned())
    }

    /// Empties the GPS directory and zeroes every value it pointed to.
    /// Returns whether there was a location.
    pub fn remove_gps(&mut self) -> bool {
        let Some(gps) = self.sub_ifd(GPS_IFD) else { return false };
        let entries = self.entries(gps);
        if entries.is_empty() {
            return false;
        }
        for e in &entries {
            if let Some(r) = self.value_range(*e) {
                self.b[r].fill(0);
            }
        }
        // The entries themselves, then a directory of none (its next-directory link follows at once).
        let end = (gps + 2 + entries.len() * 12 + 4).min(self.b.len());
        self.b[gps + 2..end].fill(0);
        self.put_u16(gps, 0);
        true
    }

    pub fn orientation(&self) -> Option<u16> {
        let e = self.find(self.ifd0()?, ORIENTATION)?;
        self.u16_at(e.at + 8)
    }
}

/// An EXIF block holding only the orientation (what "Remove all metadata"
/// keeps, or the picture would show turned).
pub fn orientation_only(orientation: u16) -> Vec<u8> {
    let mut b = Vec::with_capacity(26);
    b.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&ORIENTATION.to_be_bytes());
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&1u32.to_be_bytes());
    b.extend_from_slice(&orientation.to_be_bytes());
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&0u32.to_be_bytes());
    b
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use exif::experimental::Writer;
    use exif::{Field, In, Rational, Tag, Value};

    /// An EXIF block as a camera writes one: text fields, a date, a location.
    pub fn sample_exif(little: bool) -> Vec<u8> {
        let fields = [
            Field { tag: Tag::Make, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon".to_vec()]) },
            Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"EOS R6".to_vec()]) },
            Field { tag: Tag::Artist, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Juliano Giacomeli".to_vec()]) },
            Field { tag: Tag::Orientation, ifd_num: In::PRIMARY, value: Value::Short(vec![6]) },
            Field { tag: Tag::DateTimeOriginal, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"2024:07:01 18:30:00".to_vec()]) },
            Field { tag: Tag::FNumber, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 28, denom: 10 }]) },
            Field { tag: Tag::GPSLatitudeRef, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"S".to_vec()]) },
            Field {
                tag: Tag::GPSLatitude,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![Rational { num: 25, denom: 1 }, Rational { num: 26, denom: 1 }, Rational { num: 3000, denom: 100 }]),
            },
            Field { tag: Tag::GPSLongitudeRef, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"W".to_vec()]) },
            Field {
                tag: Tag::GPSLongitude,
                ifd_num: In::PRIMARY,
                value: Value::Rational(vec![Rational { num: 49, denom: 1 }, Rational { num: 16, denom: 1 }, Rational { num: 0, denom: 1 }]),
            },
        ];
        let mut w = Writer::new();
        for f in &fields {
            w.push_field(f);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        w.write(&mut out, little).unwrap();
        out.into_inner()
    }

    fn parsed(b: &[u8]) -> exif::Exif {
        exif::Reader::new().read_raw(b.to_vec()).unwrap()
    }

    #[test]
    fn text_fields_change_in_place_or_empty_out() {
        for little in [true, false] {
            let mut b = sample_exif(little);
            let len = b.len();
            let mut t = Tiff::new(&mut b).unwrap();
            assert_eq!(t.text(ARTIST).as_deref(), Some("Juliano Giacomeli"));
            assert!(t.set_text(ARTIST, "Ana"));
            assert!(t.set_text(DATE_TIME_ORIGINAL, "2025:01:02 03:04:05"));
            assert!(!t.set_text(COPYRIGHT, "x"), "absent: not added");
            assert_eq!(t.text(ARTIST).as_deref(), Some("Ana"));
            // Longer than its room: emptied, so the XMP value is the one read.
            assert!(t.set_text(ARTIST, "A name much longer than seventeen"));
            assert_eq!(t.text(ARTIST).as_deref(), Some(""));
            assert_eq!(t.orientation(), Some(6));
            assert_eq!(b.len(), len, "nothing moved");
            let e = parsed(&b);
            assert_eq!(e.get_field(Tag::DateTimeOriginal, In::PRIMARY).unwrap().display_value().to_string(), "2025-01-02 03:04:05");
            assert_eq!(e.get_field(Tag::Model, In::PRIMARY).unwrap().display_value().to_string(), "\"EOS R6\"");
        }
    }

    #[test]
    fn the_location_is_gone_from_the_bytes() {
        let mut b = sample_exif(false);
        let before = parsed(&b);
        assert!(before.get_field(Tag::GPSLatitude, In::PRIMARY).is_some());
        assert!(Tiff::new(&mut b).unwrap().remove_gps());
        let after = parsed(&b);
        assert!(after.get_field(Tag::GPSLatitude, In::PRIMARY).is_none());
        assert!(after.get_field(Tag::GPSLongitudeRef, In::PRIMARY).is_none());
        assert!(after.get_field(Tag::FNumber, In::PRIMARY).is_some(), "the rest stays");
        // The latitude's rationals (25, 26, 30) are no longer anywhere in the block.
        assert!(!b.windows(8).any(|w| w == [0, 0, 0, 25, 0, 0, 0, 1]));
        assert!(!Tiff::new(&mut b).unwrap().remove_gps(), "nothing left to remove");
    }

    #[test]
    fn a_block_of_only_the_orientation_reads_back() {
        let b = orientation_only(8);
        let e = parsed(&b);
        assert_eq!(e.get_field(Tag::Orientation, In::PRIMARY).unwrap().value.get_uint(0), Some(8));
        assert_eq!(e.fields().count(), 1);
    }
}
