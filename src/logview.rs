//! Reading the daemon's log for the Control Center: parsing the
//! flexi_logger detailed format, reading the end of a large file cheaply, and
//! the log categories (areas of the daemon) shared with `log_spec`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;

/// Log categories: a friendly name and the module prefixes it covers. The
/// same table drives the logger filter (`crate::log_spec`), so what the
/// Control Center shows as a category is exactly what a category level
/// controls.
pub const CATEGORIES: [(&str, &[&str]); 7] = [
    ("sync", &["git_drive::sync", "git_drive::journal"]),
    ("git", &["git_drive::git"]),
    ("fuse", &["git_drive::fs", "git_drive::range_read", "fuser"]),
    ("crypto", &["git_drive::crypt", "git_drive::chunk"]),
    ("p2p", &["git_drive::peer"]),
    (
        "network",
        &[
            "iroh", "iroh_relay", "iroh_dns", "iroh_base", "noq", "noq_proto", "noq_udp",
            "portmapper", "netwatch", "tracing",
        ],
    ),
    (
        "service",
        &[
            "git_drive::service", "git_drive::launchd", "git_drive::systemd", "git_drive::status",
            "git_drive::notify", "git_drive::tray", "git_drive::paths", "git_drive::config",
            "git_drive::control", "git_drive::viewer", "git_drive",
        ],
    ),
];

/// One record of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogEntry {
    /// `YYYY-MM-DD HH:MM:SS`, local time as written by the daemon.
    pub time: String,
    pub level: String,
    pub module: String,
    pub category: String,
    pub message: String,
}

/// The category of a module: the one with the longest matching prefix
/// (`git_drive::peer::node` is `p2p`, bare `git_drive` is `service`).
pub fn category_of(module: &str) -> &'static str {
    let mut best: Option<(&'static str, usize)> = None;
    for (name, prefixes) in CATEGORIES {
        for prefix in prefixes {
            let matches = module == *prefix
                || module.strip_prefix(prefix).is_some_and(|rest| rest.starts_with("::"));
            if matches && best.is_none_or(|(_, len)| prefix.len() > len) {
                best = Some((name, prefix.len()));
            }
        }
    }
    best.map(|(name, _)| name).unwrap_or("service")
}

/// Parses `[ts] LEVEL [module] file:line: message`. `None` for a line that
/// does not open a record (a continuation such as a backtrace frame).
pub fn parse_line(line: &str) -> Option<LogEntry> {
    let rest = line.strip_prefix('[')?;
    let (ts, rest) = rest.split_once("] ")?;
    let (level, rest) = rest.split_once(" [")?;
    let (module, rest) = rest.split_once("] ")?;
    if level.is_empty() || level.contains(' ') || ts.len() < 19 {
        return None;
    }
    // Drop the source location (`src/sync.rs:430: `) when there is one.
    let message = match rest.find(": ") {
        Some(idx) if rest[..idx].rsplit_once(':').is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) => {
            &rest[idx + 2..]
        }
        _ => rest,
    };
    Some(LogEntry {
        time: ts[..19].to_string(),
        level: level.to_string(),
        module: module.to_string(),
        category: category_of(module).to_string(),
        message: message.to_string(),
    })
}

/// Parses a chunk of log text; continuation lines join the previous entry.
pub fn parse(text: &str) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = Vec::new();
    for line in text.lines() {
        match parse_line(line) {
            Some(entry) => out.push(entry),
            None => {
                if let Some(last) = out.last_mut() {
                    last.message.push('\n');
                    last.message.push_str(line);
                }
            }
        }
    }
    out
}

/// The last `max_entries` entries of the log at `path`, reading only the end
/// of the file (the log can reach tens of MB before it rotates).
pub fn tail(path: &Path, max_entries: usize) -> std::io::Result<Vec<LogEntry>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    // ~400 bytes per line on average, with room for multi-line entries.
    let window = (max_entries as u64 * 400).max(64 * 1024).min(len);
    file.seek(SeekFrom::Start(len - window))?;
    let mut bytes = Vec::with_capacity(window as usize);
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // The window may start mid-line: skip to the first full line.
    let text = if window < len {
        text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        &text
    };
    let mut entries = parse(text);
    let skip = entries.len().saturating_sub(max_entries);
    Ok(entries.split_off(skip))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAEMON: &str = "[2026-10-14 22:24:13.700999 -03:00] INFO [git_drive::peer::node] src/peer/node.rs:192: p2p: endpoint up, device id b7166e7e04";
    const IROH: &str = "[2026-10-14 22:01:05.944131 -03:00] INFO [iroh::socket::transports] /Users/j/.cargo/registry/src/x/iroh-1.3.0/src/socket/transports.rs:964: poll_send; len=35";

    #[test]
    fn parses_the_detailed_format_and_drops_the_source_location() {
        let e = parse_line(DAEMON).unwrap();
        assert_eq!(e.time, "2026-10-14 22:24:13");
        assert_eq!(e.level, "INFO");
        assert_eq!(e.module, "git_drive::peer::node");
        assert_eq!(e.category, "p2p");
        assert_eq!(e.message, "p2p: endpoint up, device id b7166e7e04");
        let e = parse_line(IROH).unwrap();
        assert_eq!(e.category, "network");
        assert_eq!(e.message, "poll_send; len=35");
        assert!(parse_line("   1: git_drive::install_panic_hook").is_none(), "continuation");
    }

    #[test]
    fn continuation_lines_join_the_previous_entry() {
        let text = format!("{}\n   1: frame one\n   2: frame two\n{}\n", DAEMON, IROH);
        let entries = parse(&text);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].message.ends_with("b7166e7e04\n   1: frame one\n   2: frame two"));
    }

    #[test]
    fn categories_follow_the_most_specific_module() {
        assert_eq!(category_of("git_drive::sync"), "sync");
        assert_eq!(category_of("git_drive::journal"), "sync");
        assert_eq!(category_of("git_drive::git"), "git");
        assert_eq!(category_of("git_drive::fs"), "fuse");
        assert_eq!(category_of("fuser::session"), "fuse");
        assert_eq!(category_of("git_drive::crypt"), "crypto");
        assert_eq!(category_of("git_drive::peer::channel"), "p2p");
        assert_eq!(category_of("portmapper"), "network");
        assert_eq!(category_of("git_drive"), "service");
        assert_eq!(category_of("git_drive::launchd"), "service");
        assert_eq!(category_of("something_else"), "service");
    }

    #[test]
    fn tail_returns_the_last_entries_of_a_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.log");
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!(
                "[2026-10-14 10:00:00.000000 -03:00] INFO [git_drive::sync] src/sync.rs:1: line {}\n",
                i
            ));
        }
        std::fs::write(&path, text).unwrap();
        let last = tail(&path, 3).unwrap();
        let messages: Vec<&str> = last.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, vec!["line 4997", "line 4998", "line 4999"]);
    }
}
