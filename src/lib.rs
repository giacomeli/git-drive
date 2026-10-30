pub mod chunk;
pub mod config;
pub mod control;
pub mod crypt;
pub mod fs;
pub mod git;
pub mod journal;
pub mod logview;
pub mod launchd;
pub mod notify;
pub mod peer;
pub mod range_read;
pub mod reserved;
pub mod service;
pub mod sync;
// The observable state lives in its own crate, which the apps link alone
// (no FUSE, no Git); these keep its modules under their old paths.
pub use git_drive_state::{paths, status, storage};
pub mod systemd;
pub mod tray;
pub mod viewer;

use std::path::{Path, PathBuf};

/// Logger specification for the configured base level and per-category
/// levels (`log_categories`, see `logview::CATEGORIES`). Every module of every
/// category is listed explicitly, so a category level never leaks into
/// another category through a shared prefix (`git_drive` vs
/// `git_drive::sync`). The `network` category (the iroh stack and its tracing
/// span records) defaults to warnings: it logs internal events at INFO
/// several times per second. Invalid levels fall back to the base level.
pub fn log_spec(level: &str, categories: &std::collections::BTreeMap<String, String>) -> String {
    const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];
    let mut spec = level.to_string();
    for (name, modules) in logview::CATEGORIES {
        let explicit = categories
            .get(name)
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| LEVELS.contains(&l.as_str()));
        let category_level = explicit.unwrap_or_else(|| {
            if name == "network" { "warn".to_string() } else { level.to_string() }
        });
        for module in modules.iter() {
            spec.push_str(&format!(", {}={}", module, category_level));
        }
    }
    spec
}

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
    categories: &std::collections::BTreeMap<String, String>,
    max_mb: u64,
    keep_files: usize,
) -> anyhow::Result<(flexi_logger::LoggerHandle, PathBuf)> {
    use flexi_logger::{Cleanup, Criterion, Duplicate, FileSpec, Logger, Naming, WriteMode};

    std::fs::create_dir_all(log_dir)?;

    let mut logger = Logger::try_with_env_or_str(log_spec(level, categories))?
        .log_to_file(FileSpec::default().directory(log_dir).basename("git-drive"))
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
        newest_log_file(log_dir).unwrap_or_else(|| log_dir.join("git-drive_rCURRENT.log"));
    Ok((handle, current))
}

/// Newest `git-drive*.log` in `dir` by modification time. At startup this is
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
                .is_some_and(|n| n.starts_with("git-drive") && n.ends_with(".log"))
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
    // does so. `keep` leaves the dir alive past this test (the global
    // logger keeps pointing at it), avoiding write-after-delete noise.
    #[test]
    fn log_categories_set_levels_per_area_of_the_daemon() {
        let mut cats = std::collections::BTreeMap::new();
        cats.insert("p2p".to_string(), "debug".to_string());
        cats.insert("fuse".to_string(), "off".to_string());
        cats.insert("network".to_string(), "info".to_string());
        cats.insert("git".to_string(), "loud".to_string());
        let spec = log_spec("info", &cats);
        assert!(spec.contains("git_drive::peer=debug"), "{}", spec);
        assert!(spec.contains("fuser=off") && spec.contains("git_drive::fs=off"), "{}", spec);
        assert!(spec.contains("iroh=info") && spec.contains("tracing=info"), "{}", spec);
        assert!(spec.contains("git_drive::git=info"), "an invalid level falls back: {}", spec);
        assert!(spec.contains("git_drive::sync=info"), "untouched categories keep the base level: {}", spec);
        assert!(flexi_logger::LogSpecification::parse(&spec).is_ok(), "{}", spec);
    }

    #[test]
    fn the_network_stack_logs_only_warnings_by_default() {
        // iroh logs internal events at INFO several times per second (1,776
        // lines in 8 minutes on the Mac, 14-10-2026): they stay out of the
        // drive's log unless RUST_LOG asks for them.
        for level in ["info", "debug"] {
            let spec = log_spec(level, &Default::default());
            assert!(spec.starts_with(level), "{}", spec);
            // Span lifecycle records of tracing-based crates (iroh) carry the
            // target "tracing::span", not the crate's module.
            for noisy in ["iroh=warn", "iroh_relay=warn", "noq=warn", "portmapper=warn", "tracing=warn"] {
                assert!(spec.contains(noisy), "{} missing in {}", noisy, spec);
            }
            assert!(flexi_logger::LogSpecification::parse(&spec).is_ok(), "{}", spec);
        }
    }

    #[test]
    #[serial_test::serial]
    fn init_logging_creates_and_locates_current_file() {
        let dir = tempfile::tempdir().unwrap().keep();
        let (handle, path) = init_logging(false, &dir, "info", &Default::default(), 50, 10).unwrap();
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
