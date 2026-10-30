//! Thumbnails for the icon view. Raster images are decoded and scaled in
//! process (`image`); on macOS whatever Quick Look can draw (video, PDF, HEIC,
//! office documents) goes through `qlmanage`. Only bytes come out: nothing is
//! cached on disk, so the content of an encrypted drive never lands outside
//! it in clear.

use std::io::Cursor;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, ExitStatus, Stdio};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

/// How a file's thumbnail is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Decoded and scaled here.
    Raster,
    /// Drawn by the system's Quick Look.
    #[cfg(target_os = "macos")]
    QuickLook,
}

/// Raster files above this are not decoded here (a 24-megapixel photo is
/// ~10 MB; a bigger file is a scan or a poster): Quick Look draws them on
/// macOS, and they keep their icon on Linux.
pub const RASTER_MAX_BYTES: u64 = 64 << 20;

/// A Quick Look helper that has not answered by then is killed (`qlmanage`
/// hangs on some files, a Rust source among them).
#[cfg(target_os = "macos")]
pub const QUICKLOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Decoded here on every platform.
const RASTER: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// What Quick Look draws better than an icon. Text and code are left out:
/// their thumbnails are tiny pages of unreadable lines, and `qlmanage` hangs
/// on some of them. SVG too: the webview draws it itself, keeping the
/// transparency Quick Look paints white.
#[cfg(target_os = "macos")]
const QUICKLOOK: &[&str] = &[
    "mp4", "m4v", "mov", "pdf", "heic", "heif", "tif", "tiff", "psd", "avif", "docx", "xlsx", "pptx", "pages",
    "numbers", "key",
];

/// JPEG quality of opaque thumbnails: indistinguishable at icon size, about
/// a fifth of the PNG.
const JPEG_QUALITY: u8 = 82;

/// The thumbnail source for a file with this (lowercase) extension and size;
/// `None` keeps the file's icon.
pub fn source_for(ext: &str, size: u64) -> Option<Source> {
    if RASTER.contains(&ext) && size <= RASTER_MAX_BYTES {
        return Some(Source::Raster);
    }
    #[cfg(target_os = "macos")]
    if RASTER.contains(&ext) || QUICKLOOK.contains(&ext) {
        return Some(Source::QuickLook);
    }
    None
}

/// `path` scaled to fit `max` x `max` (never enlarged), upright per its
/// orientation tag: PNG when it has transparency, JPEG otherwise.
pub fn raster(path: &Path, max: u32) -> Result<Vec<u8>, String> {
    let fail = |e: image::ImageError| format!("{}: {e}", path.display());
    let reader = ImageReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The content decides the format: a photo saved with the wrong extension still shows.
    let mut decoder = reader
        .with_guessed_format()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .into_decoder()
        .map_err(fail)?;
    let orientation = decoder.orientation().map_err(fail)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(fail)?;
    img.apply_orientation(orientation);
    if img.width() > max || img.height() > max {
        img = img.thumbnail(max, max);
    }
    let mut out = Cursor::new(Vec::new());
    if img.color().has_alpha() {
        img.write_to(&mut out, ImageFormat::Png).map_err(fail)?;
    } else {
        img.to_rgb8().write_with_encoder(JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)).map_err(fail)?;
    }
    Ok(out.into_inner())
}

/// Runs `cmd`, killing it once `timeout` passes.
#[cfg(target_os = "macos")]
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<ExitStatus, String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer after {} s", timeout.as_secs_f32()));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Quick Look's PNG thumbnail of `path`, at most `max` pixels on a side;
/// `None` when it has none for this file.
#[cfg(target_os = "macos")]
pub fn quicklook(path: &Path, max: u32, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
    let name = path.file_name().ok_or_else(|| format!("{} has no file name", path.display()))?;
    // One scratch folder per call: several thumbnails are drawn at once.
    let out = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut cmd = Command::new("qlmanage");
    cmd.args(["-t", "-s", &max.to_string(), "-o"]).arg(out.path()).arg(path);
    run_with_timeout(&mut cmd, timeout).map_err(|e| format!("Quick Look, {}: {e}", path.display()))?;
    let mut png = name.to_os_string();
    png.push(".png");
    match std::fs::read(out.path().join(png)) {
        Ok(bytes) => Ok(Some(bytes)),
        // qlmanage exits 0 without a file when it has no thumbnail for this kind.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// The thumbnail of `path` at most `max` pixels on a side, or `None` when the
/// file keeps its icon. With `use_quicklook` off, only what is decoded here
/// is drawn, and a picture this decoder cannot read is an error (a folder's
/// previews on the drive: Quick Look writes a PNG of the file to `$TMPDIR`,
/// on disk).
pub fn thumbnail_with(path: &Path, max: u32, use_quicklook: bool) -> Result<Option<Vec<u8>>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    #[cfg(target_os = "macos")]
    if !use_quicklook {
        return match source_for(&ext, meta.len()) {
            Some(Source::Raster) => raster(path, max).map(Some),
            _ => Ok(None),
        };
    }
    #[cfg(not(target_os = "macos"))]
    let _ = use_quicklook;
    match source_for(&ext, meta.len()) {
        None => Ok(None),
        // An image this decoder cannot read (a rare JPEG flavor, say) may still be one Quick Look can.
        #[cfg(target_os = "macos")]
        Some(Source::Raster) => raster(path, max).map(Some).or_else(|_| quicklook(path, max, QUICKLOOK_TIMEOUT)),
        #[cfg(not(target_os = "macos"))]
        Some(Source::Raster) => raster(path, max).map(Some),
        #[cfg(target_os = "macos")]
        Some(Source::QuickLook) => quicklook(path, max, QUICKLOOK_TIMEOUT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    fn decode(bytes: &[u8]) -> (ImageFormat, (u32, u32)) {
        let format = image::guess_format(bytes).unwrap();
        (format, image::load_from_memory(bytes).unwrap().dimensions())
    }

    #[test]
    fn sources_follow_the_extension_size_and_platform() {
        assert_eq!(source_for("png", 1000), Some(Source::Raster));
        assert_eq!(source_for("jpeg", 1000), Some(Source::Raster));
        assert_eq!(source_for("rs", 1000), None, "code keeps its icon");
        assert_eq!(source_for("", 1000), None);
        #[cfg(target_os = "macos")]
        let quicklook = Some(Source::QuickLook);
        #[cfg(not(target_os = "macos"))]
        let quicklook = None;
        assert_eq!(source_for("png", RASTER_MAX_BYTES + 1), quicklook, "too big to decode here");
        assert_eq!(source_for("mp4", 1000), quicklook);
        assert_eq!(source_for("pdf", 1000), quicklook);
        assert_eq!(source_for("heic", 1000), quicklook);
        assert_eq!(source_for("svg", 1000), None, "the webview draws SVG itself");
    }

    #[test]
    fn raster_thumbnails_fit_the_bound_and_keep_the_aspect_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        RgbImage::from_pixel(400, 200, Rgb([200, 40, 90])).save(&path).unwrap();
        let (format, dims) = decode(&raster(&path, 128).unwrap());
        assert_eq!(format, ImageFormat::Jpeg, "opaque images become JPEG");
        assert_eq!(dims, (128, 64));
    }

    #[test]
    fn transparent_images_stay_png_and_are_never_enlarged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logo.png");
        RgbaImage::from_pixel(50, 80, Rgba([10, 20, 30, 100])).save(&path).unwrap();
        let (format, dims) = decode(&raster(&path, 128).unwrap());
        assert_eq!(format, ImageFormat::Png);
        assert_eq!(dims, (50, 80));
    }

    #[test]
    fn a_broken_image_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.png");
        std::fs::write(&path, "not an image").unwrap();
        assert!(raster(&path, 128).is_err());
        assert!(thumbnail_with(&dir.path().join("missing.png"), 128, true).is_err());
    }

    #[test]
    fn without_quick_look_only_pictures_decoded_here_are_drawn() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.png");
        RgbImage::from_pixel(40, 30, Rgb([1, 2, 3])).save(&photo).unwrap();
        assert_eq!(decode(&thumbnail_with(&photo, 128, false).unwrap().unwrap()).1, (40, 30));
        // A Quick Look kind keeps its icon, and a picture this decoder cannot read is an error
        // (never handed to Quick Look, which writes its PNG to disk).
        let doc = dir.path().join("doc.pdf");
        std::fs::write(&doc, "%PDF-1.4").unwrap();
        assert_eq!(thumbnail_with(&doc, 128, false).unwrap(), None);
        let broken = dir.path().join("broken.jpg");
        std::fs::write(&broken, "not a jpeg").unwrap();
        assert!(thumbnail_with(&broken, 128, false).is_err());
    }

    #[test]
    fn files_without_a_source_keep_their_icon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        assert_eq!(thumbnail_with(&path, 128, true).unwrap(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_hung_helper_is_killed_at_the_timeout() {
        let start = Instant::now();
        let result = run_with_timeout(Command::new("sleep").arg("5"), Duration::from_millis(200));
        assert!(result.is_err(), "timed out");
        assert!(start.elapsed() < Duration::from_secs(2), "killed, not waited for");
        assert!(run_with_timeout(&mut Command::new("true"), Duration::from_secs(5)).unwrap().success());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quicklook_draws_a_bounded_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("square.png");
        RgbImage::from_pixel(300, 300, Rgb([40, 160, 220])).save(&path).unwrap();
        let bytes = quicklook(&path, 128, QUICKLOOK_TIMEOUT).unwrap().expect("Quick Look draws a PNG");
        let (format, (w, h)) = decode(&bytes);
        assert_eq!(format, ImageFormat::Png);
        assert!(w <= 128 && h <= 128, "{w}x{h}");
    }
}
