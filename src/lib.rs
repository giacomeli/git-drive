pub mod chunk;
pub mod config;
pub mod crypt;
pub mod fs;
pub mod git;
pub mod launchd;
pub mod notify;
pub mod paths;
pub mod status;
pub mod sync;
pub mod tray;
pub mod viewer;

use std::path::{Path, PathBuf};

/// Initializes the process-wide logger (flexi_logger) writing to rotating
/// files in `log_dir`. Returns the logger handle (which must stay alive for
/// the whole run) and the path of the current (active) log file, so the
/// status/tray/viewer can tail it.
///
/// - `level` (e.g. "info") is the default filter, overridable at runtime by
///   `RUST_LOG` — this also captures the `log` output of fuser and other libs
///   in the same place (single backend, replacing env_logger).
/// - `max_mb > 0` enables size-based rotation, keeping at most `keep_files`
///   rotated files. This bounds disk use, unlike the old unbounded single-file
///   redirect (which reached 225 MB in a single session).
/// - `verbose` mirrors every record to stderr (terminal), like the old tee.
pub fn init_logging(
    verbose: bool,
    log_dir: &Path,
    level: &str,
    max_mb: u64,
    keep_files: usize,
) -> anyhow::Result<(flexi_logger::LoggerHandle, PathBuf)> {
    use flexi_logger::{Cleanup, Criterion, Duplicate, FileSpec, Logger, Naming, WriteMode};

    std::fs::create_dir_all(log_dir)?;

    let mut logger = Logger::try_with_env_or_str(level)?
        .log_to_file(FileSpec::default().directory(log_dir).basename("drive-sync"))
        .format_for_files(flexi_logger::detailed_format)
        // Direct write keeps the tail (viewer / `tail -f`) live without waiting
        // for a periodic flush.
        .write_mode(WriteMode::Direct);

    if max_mb > 0 {
        logger = logger.rotate(
            Criterion::Size(max_mb.saturating_mul(1024 * 1024)),
            Naming::Timestamps,
            Cleanup::KeepLogFiles(keep_files),
        );
    }
    if verbose {
        logger = logger.duplicate_to_stderr(Duplicate::All);
    }

    let handle = logger.start()?;
    // Emit one line so the current file exists, then locate it — robust to
    // flexi_logger's file-naming convention across versions/rotation modes.
    log::info!(
        "boot: logging initialized (level={}, rotate={} MB, keep={})",
        level,
        max_mb,
        keep_files
    );
    let current =
        newest_log_file(log_dir).unwrap_or_else(|| log_dir.join("drive-sync_rCURRENT.log"));
    Ok((handle, current))
}

/// Newest `drive-sync*.log` in `dir` by modification time. At startup this is
/// the file flexi_logger just created (the active one), whatever its exact
/// name.
fn newest_log_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("drive-sync") && n.ends_with(".log"))
        })
        .max_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Initializes the process-global logger, so it must be the only test that
    // does so. `into_path` keeps the dir alive past this test (the global
    // logger keeps pointing at it), avoiding write-after-delete noise.
    #[test]
    #[serial_test::serial]
    fn init_logging_creates_and_locates_current_file() {
        let dir = tempfile::tempdir().unwrap().into_path();
        let (handle, path) = init_logging(false, &dir, "info", 50, 10).unwrap();
        log::info!("test line for the current file");
        handle.flush();
        assert!(
            path.exists(),
            "the located current log file should exist: {:?}",
            path
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("logging initialized"),
            "boot line should be in the located file:\n{}",
            content
        );
    }
}
