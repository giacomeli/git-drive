//! Set Desktop Picture: every screen on macOS (NSWorkspace, which needs the
//! main thread for the screens and no permission), GNOME's background on
//! Linux (`gsettings`, for the light and the dark style).

use std::path::Path;

/// `text` as a GVariant string literal, the form `gsettings set` reads.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn gvariant_string(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(target_os = "macos")]
pub fn set(path: &Path, mtm: objc2::MainThreadMarker) -> Result<(), String> {
    use objc2_app_kit::{NSScreen, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let ws = NSWorkspace::sharedWorkspace();
    for screen in NSScreen::screens(mtm).iter() {
        // SAFETY: an empty options dictionary keeps each screen's scaling.
        unsafe { ws.setDesktopImageURL_forScreen_options_error(&url, &screen, &NSDictionary::new()) }
            .map_err(|e| t!("wallpaper.macos_refused", reason = e.localizedDescription()))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn set(path: &Path) -> Result<(), String> {
    use gtk::gio::prelude::*;
    let uri = gtk::gio::File::for_path(path).uri().to_string();
    for key in ["picture-uri", "picture-uri-dark"] {
        let out = std::process::Command::new("gsettings")
            .args(["set", "org.gnome.desktop.background", key])
            .arg(gvariant_string(&uri))
            .output()
            .map_err(|_| t!("wallpaper.needs_gnome"))?;
        if !out.status.success() {
            return Err(t!("wallpaper.gnome_refused", reason = String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_becomes_a_gvariant_string() {
        assert_eq!(gvariant_string("file:///home/u/a%20b.jpg"), "'file:///home/u/a%20b.jpg'");
        assert_eq!(gvariant_string("file:///home/u/it's.jpg"), r"'file:///home/u/it\'s.jpg'");
    }
}
