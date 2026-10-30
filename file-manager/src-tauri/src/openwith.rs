//! Open With: the apps that open a file (its default app first), with their
//! icons; opening files with one of them, and making one the default for the
//! file's type. macOS: NSWorkspace (no permission needed); Linux: GIO's app
//! infos and the GTK icon theme, which only work on the main thread.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Serialize;

/// Side of an app's icon, in pixels (shown at 16 points, sharp at 2x).
const ICON_PX: i32 = 32;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AppChoice {
    /// What `open_with` takes: the app bundle's path on macOS, the desktop
    /// file id (`org.gnome.Loupe.desktop`) on Linux.
    pub id: String,
    pub name: String,
    /// The app's icon as a PNG data URL, when one could be drawn.
    pub icon: Option<String>,
    /// The app the system opens this file with.
    pub default: bool,
}

pub fn png_data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
}

/// Where an app is, to tell two of the same name apart: the folder of a
/// bundle (home as `~`), or a desktop file id without `.desktop`.
fn whereabouts(id: &str) -> String {
    match Path::new(id).parent().filter(|p| p.is_absolute()) {
        Some(dir) => {
            let dir = dir.display().to_string();
            match std::env::var("HOME") {
                Ok(home) if dir.starts_with(&home) => format!("~{}", &dir[home.len()..]),
                _ => dir,
            }
        }
        None => id.trim_end_matches(".desktop").to_string(),
    }
}

/// The default app first, then the others by name; one entry per id, and
/// two apps of the same name tell where each one is.
pub fn ordered(mut apps: Vec<AppChoice>) -> Vec<AppChoice> {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert(a.id.clone()));
    let mut count = std::collections::HashMap::new();
    for a in &apps {
        *count.entry(a.name.clone()).or_insert(0) += 1;
    }
    for a in apps.iter_mut().filter(|a| count[&a.name] > 1) {
        a.name = format!("{} ({})", a.name, whereabouts(&a.id));
    }
    apps.sort_by(|a, b| b.default.cmp(&a.default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    apps
}

pub use imp::{apps_for, open_with, set_default};

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2::rc::Retained;
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSFileManager, NSPoint, NSRect, NSSize, NSString, NSURL};

    fn file_url(path: &Path) -> Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
    }

    /// Draws `image` into a square bitmap and encodes it as PNG.
    fn png_of(image: &NSImage, px: i32) -> Option<Vec<u8>> {
        // SAFETY: a null planes pointer lets the rep allocate its own buffer;
        // the color space name is AppKit's own constant.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                px as isize,
                px as isize,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                0,
                0,
            )
        }?;
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        image.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64)));
        ctx.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        // SAFETY: an empty properties dictionary is valid for PNG.
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(data.to_vec())
    }

    /// The apps that open `path`, as Finder's Open With lists them.
    pub fn apps_for(path: &Path) -> Vec<AppChoice> {
        let ws = NSWorkspace::sharedWorkspace();
        let url = file_url(path);
        let default = ws.URLForApplicationToOpenURL(&url).and_then(|u| u.path()).map(|p| p.to_string());
        let names = NSFileManager::defaultManager();
        let apps = ws
            .URLsForApplicationsToOpenURL(&url)
            .iter()
            .filter_map(|u| u.path())
            .map(|p| {
                let icon = png_of(&ws.iconForFile(&p), ICON_PX).map(|png| png_data_url(&png));
                let id = p.to_string();
                AppChoice { name: names.displayNameAtPath(&p).to_string().trim_end_matches(".app").to_string(), icon, default: Some(&id) == default.as_ref(), id }
            })
            .collect();
        ordered(apps)
    }

    /// Opens `paths` with the app at `app` (`open -a`, as Finder's Open With does).
    pub fn open_with(paths: &[PathBuf], app: &str) -> Result<(), String> {
        let child = std::process::Command::new("open").arg("-a").arg(app).args(paths).spawn();
        child.as_ref().map_err(|e| t!("openwith.failed", app = app, error = crate::i18n::io_text(e)))?;
        git_drive_state::reap_in_background(child);
        Ok(())
    }

    /// Makes `app` the default for files of `path`'s type (macOS 12+).
    pub fn set_default(path: &Path, app: &str) -> Result<(), String> {
        NSWorkspace::sharedWorkspace().setDefaultApplicationAtURL_toOpenContentTypeOfFileAtURL_completionHandler(
            &file_url(Path::new(app)),
            &file_url(path),
            None,
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use gtk::gio::prelude::*;
    use gtk::prelude::*;
    use gtk::{gio, IconLookupFlags, IconTheme};

    fn content_type(path: &Path) -> String {
        if path.is_dir() {
            return "inode/directory".into();
        }
        gio::content_type_guess(Some(path), &[]).0.to_string()
    }

    fn icon_png(info: &gio::AppInfo) -> Option<Vec<u8>> {
        let icon = info.icon()?;
        let looked = IconTheme::default()?.lookup_by_gicon(&icon, ICON_PX, IconLookupFlags::FORCE_SIZE)?;
        looked.load_icon().ok()?.save_to_bufferv("png", &[]).ok()
    }

    /// The apps that declare `path`'s type (main thread: GTK's icon theme).
    pub fn apps_for(path: &Path) -> Vec<AppChoice> {
        let kind = content_type(path);
        let default = gio::AppInfo::default_for_type(&kind, false).and_then(|a| a.id()).map(|id| id.to_string());
        let apps = gio::AppInfo::all_for_type(&kind)
            .into_iter()
            .filter(|a| a.should_show())
            .filter_map(|a| {
                let id = a.id()?.to_string();
                Some(AppChoice {
                    name: a.display_name().to_string(),
                    icon: icon_png(&a).map(|png| png_data_url(&png)),
                    default: Some(&id) == default.as_ref(),
                    id,
                })
            })
            .collect();
        ordered(apps)
    }

    fn app(id: &str) -> Result<gio::DesktopAppInfo, String> {
        gio::DesktopAppInfo::new(id).ok_or_else(|| t!("openwith.not_installed", app = id))
    }

    /// Opens `paths` with the app whose desktop file id is `id` (main thread).
    pub fn open_with(paths: &[PathBuf], id: &str) -> Result<(), String> {
        let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
        let context = gtk::gdk::Display::default().and_then(|d| d.app_launch_context());
        app(id)?.launch(&files, context.as_ref()).map_err(|e| e.to_string())
    }

    /// Makes `id` the default for files of `path`'s type (main thread).
    pub fn set_default(path: &Path, id: &str) -> Result<(), String> {
        app(id)?.set_as_default_for_type(&content_type(path)).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, default: bool) -> AppChoice {
        AppChoice { id: id.into(), name: name.into(), icon: None, default }
    }

    #[test]
    fn the_default_app_comes_first_then_the_rest_by_name() {
        let apps = ordered(vec![app("/z", "zed", false), app("/p", "Preview", true), app("/a", "Acorn", false), app("/z", "zed", false)]);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Preview", "Acorn", "zed"], "one entry per app");
    }

    #[test]
    fn two_copies_of_an_app_say_where_each_one_is() {
        let home = std::env::var("HOME").unwrap();
        let apps = ordered(vec![
            app("/Applications/Brave Browser.app", "Brave Browser", false),
            app(&format!("{home}/Applications/Brave Browser.app"), "Brave Browser", false),
            app("org.gnome.Loupe.desktop", "Image Viewer", false),
            app("org.kde.gwenview.desktop", "Image Viewer", false),
        ]);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["Brave Browser (/Applications)", "Brave Browser (~/Applications)", "Image Viewer (org.gnome.Loupe)", "Image Viewer (org.kde.gwenview)"]
        );
    }

    #[test]
    fn icons_travel_as_png_data_urls() {
        assert_eq!(png_data_url(&[0x89, b'P', b'N', b'G']), "data:image/png;base64,iVBORw==");
    }
}

