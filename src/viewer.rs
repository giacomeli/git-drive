//! egui window showing daemon status + a live tail of the log
//! (`drive-sync viewer`).
//!
//! Runs as its own process, spawned by the tray (decision 10 of the
//! observability plan): a separate process avoids clashing with the tray's
//! `tao` event loop and isolates a viewer crash from the status icon. Pure
//! Rust (egui/eframe) — no system webview dependency, so it works on Linux
//! (Arch) as well as macOS.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::status;

/// How often status.json and the log tail are re-read.
const POLL: Duration = Duration::from_millis(1000);
/// How many bytes to read from the end of the log (bounded work regardless of
/// the file size).
const TAIL_BYTES: u64 = 256 * 1024;
/// Maximum number of log lines kept in the view.
const MAX_LINES: usize = 2000;

struct ViewerApp {
    status_path: PathBuf,
    log_path: Option<PathBuf>,
    status_text: String,
    log_text: String,
    last_refresh: Option<Instant>,
}

impl ViewerApp {
    fn new(status_path: PathBuf, log_path: Option<PathBuf>) -> Self {
        let mut app = ViewerApp {
            status_path,
            log_path,
            status_text: String::new(),
            log_text: String::new(),
            last_refresh: None,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        match status::read_from(&self.status_path) {
            Ok(s) => {
                let alive = status::pid_alive(s.pid);
                self.status_text = status::format_status(&s, status::now_unix(), alive);
                // Derive the log path from the status when not passed explicitly.
                if self.log_path.is_none() && !s.log_file.is_empty() {
                    self.log_path = Some(PathBuf::from(&s.log_file));
                }
            }
            Err(e) => {
                self.status_text = format!("status unavailable: {}", e);
            }
        }
        if let Some(path) = &self.log_path {
            self.log_text = read_tail(path, TAIL_BYTES, MAX_LINES)
                .unwrap_or_else(|e| format!("log unavailable: {}", e));
        }
        self.last_refresh = Some(Instant::now());
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let due = match self.last_refresh {
            Some(t) => t.elapsed() >= POLL,
            None => true,
        };
        if due {
            self.refresh();
        }

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("drive-sync");
            ui.separator();
            ui.monospace(&self.status_text);
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Log");
                if let Some(p) = &self.log_path {
                    ui.weak(p.display().to_string());
                }
            });
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.monospace(&self.log_text);
                });
        });

        // Keep repainting so the tail stays live even without user input.
        ctx.request_repaint_after(POLL);
    }
}

/// Reads up to `max_bytes` from the end of `path` and returns its last
/// `max_lines` lines. A partial first line (from seeking mid-line) is dropped.
fn read_tail(path: &Path, max_bytes: u64, max_lines: usize) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let text: &str = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => &text,
        }
    } else {
        &text
    };
    let lines: Vec<&str> = text.lines().collect();
    let tail = if lines.len() > max_lines {
        &lines[lines.len() - max_lines..]
    } else {
        &lines[..]
    };
    Ok(tail.join("\n"))
}

/// Opens the viewer window. Blocks the calling thread until the window closes.
pub fn run(status_path: PathBuf, log_path: Option<PathBuf>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_title("drive-sync"),
        ..Default::default()
    };
    eframe::run_native(
        "drive-sync",
        options,
        Box::new(move |_cc| Ok(Box::new(ViewerApp::new(status_path, log_path)))),
    )
    .map_err(|e| anyhow::anyhow!("viewer failed: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_tail_returns_last_lines_and_drops_partial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..1000 {
            writeln!(f, "line {}", i).unwrap();
        }
        // Small byte budget forces a mid-line seek → partial first line dropped.
        let out = read_tail(&path, 64, 100).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() <= 100);
        assert_eq!(*lines.last().unwrap(), "line 999");
        // No partial fragment at the start.
        assert!(lines[0].starts_with("line "));
    }

    #[test]
    fn read_tail_small_file_returns_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.log");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let out = read_tail(&path, 256 * 1024, 2000).unwrap();
        assert_eq!(out, "a\nb\nc");
    }
}
