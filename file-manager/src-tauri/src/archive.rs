//! Compress and Extract Here, through the system's `bsdtar` (libarchive: on
//! macOS at /usr/bin, on Linux with libarchive). Archives land beside their
//! sources under a free name, as Finder names them; nothing is overwritten.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::ops::{free_target, rename_noreplace};
use crate::tasks::{measure, Task, CANCELLED};

/// Endings `bsdtar` reads, longest first (`.tar.gz` before `.gz`).
const ARCHIVE_ENDINGS: [&str; 12] = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".tbz", ".txz", ".zip", ".tar", ".7z", ".rar"];

/// An archive's name without its ending (`backup.tar.gz` -> `backup`); `None` when it is no archive.
pub fn archive_stem(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    let ending = ARCHIVE_ENDINGS.iter().find(|e| lower.ends_with(*e))?;
    let stem = &name[..name.len() - ending.len()];
    (!stem.is_empty()).then_some(stem)
}

/// The system's `bsdtar`, where GUI apps find it (their PATH is short on macOS).
pub fn bsdtar() -> Option<PathBuf> {
    let from_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    from_path
        .chain(["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"].map(PathBuf::from))
        .map(|dir| dir.join("bsdtar"))
        .find(|p| p.is_file())
}

/// Runs `bsdtar` (given `-v`), counting each entry it reports (`a name` as it adds,
/// `x name` as it extracts) on `task`; a cancel kills it.
fn run(cmd: &mut Command, task: &Task) -> Result<(), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| t!("archive.run_failed", error = crate::i18n::io_text(&e)))?;
    let stderr = child.stderr.take().ok_or_else(|| t!("archive.no_output"))?;
    let outcome = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let (mut errors, mut others) = (Vec::new(), Vec::new());
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                match entry_line(&line) {
                    Some((entry, error)) => {
                        task.on(Path::new(entry));
                        task.file_done();
                        if let Some(e) = error {
                            errors.push(e.to_string());
                        }
                    }
                    None if !line.trim().is_empty() => others.push(line),
                    None => {}
                }
            }
            (errors, others)
        });
        loop {
            if task.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                break Err(CANCELLED.to_string());
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => break Err(t!("archive.wait_failed", error = crate::i18n::io_text(&e))),
            }
        }
        .map(|status| (status, reader.join().unwrap_or_default()))
    })?;
    let (status, (errors, others)) = outcome;
    if status.success() {
        return Ok(());
    }
    Err(failure_text(&errors, &others))
}

/// How libarchive's messages about an entry begin. `bsdtar -v` writes them on
/// the entry's own line (`x name: Can't create 'name': ...`); a colon inside a
/// name is not followed by one of these.
const ENTRY_ERRORS: &[&str] = &[
    "Can't", "Cannot", "Could not", "Failed", "Refusing", "Path ", "Hard-link", "Linkname", "Truncated", "Write ", "Damaged", "Unsupported", "Invalid", "Error",
];

/// A `-v` line about an entry (`x name` as it extracts, `a name` as it adds):
/// the entry, and the error bsdtar appended to it; `None` for any other line.
fn entry_line(line: &str) -> Option<(&str, Option<&str>)> {
    let rest = line.strip_prefix("x ").or_else(|| line.strip_prefix("a "))?;
    let mut from = 0;
    while let Some(at) = rest[from..].find(": ").map(|i| from + i) {
        let message = &rest[at + 2..];
        if ENTRY_ERRORS.iter().any(|e| message.starts_with(e)) {
            return Some((&rest[..at], Some(message)));
        }
        from = at + 2;
    }
    Some((rest, None))
}

/// What a failed run says: the first entry's error (and how many more), else
/// bsdtar's own first line ("Error exit delayed from previous errors." only
/// sums up the entries' errors).
fn failure_text(errors: &[String], others: &[String]) -> String {
    if let Some(first) = errors.first() {
        return if errors.len() > 1 { t!("archive.and_more", error = first, count = errors.len() - 1) } else { first.clone() };
    }
    let first = others.first().map_or("bsdtar failed", |l| l.trim());
    first.strip_prefix("bsdtar: ").unwrap_or(first).to_string()
}

/// Entries an extraction leaves out: macOS's own metadata (Finder's view
/// settings, AppleDouble files and the `__MACOSX` folder that holds them). The
/// drive is mounted with macFUSE's `noappledouble`, so creating them there
/// fails, and one such entry failed the whole extraction.
const JUNK: &[&str] = &[".DS_Store", "._*", "__MACOSX"];

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// Zips `paths` (items of one folder) beside them: `name.zip` for one item,
/// `Archive.zip` for several, under a free name. Written to a temporary
/// file first, so a failure or a cancel leaves nothing behind. Returns the archive.
pub fn compress(paths: &[PathBuf], task: &Task) -> Result<PathBuf, String> {
    let first = paths.first().ok_or_else(|| t!("archive.nothing"))?;
    let dir = first.parent().ok_or_else(|| t!("archive.root_no_folder"))?;
    if paths.iter().any(|p| p.parent() != Some(dir)) {
        return Err(t!("archive.different_folders"));
    }
    let tool = bsdtar().ok_or_else(|| t!("archive.compress_needs_bsdtar"))?;
    let (bytes, files) = measure(paths, task).map_err(|_| CANCELLED.to_string())?;
    task.set_totals(bytes, files, paths.len() as u64);
    let name = if paths.len() == 1 { format!("{}.zip", name_of(first)) } else { format!("{}.zip", t!("archive.default_name")) };
    // `-a` picks the format from the ending, so the temporary name keeps `.zip`.
    let partial = dir.join(format!(".{name}.partial-{}.zip", std::process::id()));
    let mut cmd = Command::new(tool);
    cmd.args(["-a", "-c", "-v", "-f"]).arg(&partial).args(["--exclude", ".DS_Store"]);
    if cfg!(target_os = "macos") {
        // No AppleDouble `._` entries: the archive reads the same on Linux.
        cmd.arg("--no-mac-metadata");
    }
    cmd.arg("-C").arg(dir).args(paths.iter().map(|p| name_of(p)));
    let target = run(&mut cmd, task).and_then(|_| {
        let target = free_target(dir, &name, false, false);
        rename_noreplace(&partial, &target).map(|_| target).map_err(|e| t!("archive.name_failed", error = crate::i18n::io_text(&e)))
    });
    if target.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    target
}

/// Extracts `archive` beside it, as Finder does: a single item lands under
/// its own (free) name, several go in a folder named after the archive. The
/// entries go to a fresh folder first, so an existing item is never merged
/// into or replaced; `bsdtar` refuses entries that climb out (`..`, absolute
/// paths) or go through a link. Returns where the items landed. The number
/// of entries is not known before the end: the window shows them as they come.
pub fn extract(archive: &Path, task: &Task) -> Result<Vec<PathBuf>, String> {
    let dir = archive.parent().ok_or_else(|| t!("archive.no_folder"))?;
    let name = name_of(archive);
    let stem = archive_stem(&name).ok_or_else(|| t!("archive.not_an_archive", name = name))?;
    let tool = bsdtar().ok_or_else(|| t!("archive.extract_needs_bsdtar"))?;
    let scratch = dir.join(format!(".{name}.extracting-{}", std::process::id()));
    std::fs::create_dir(&scratch).map_err(|e| t!("archive.scratch_failed", error = crate::i18n::io_text(&e)))?;
    task.set_totals(0, 0, 1);
    let mut cmd = Command::new(tool);
    cmd.args(["-x", "-v", "-f"]).arg(archive).arg("-C").arg(&scratch);
    for pattern in JUNK {
        cmd.args(["--exclude", pattern]);
    }
    if cfg!(target_os = "macos") {
        // AppleDouble metadata is not restored either (it needs the `._` entries left out
        // above, and failed a whole extraction on a stray one), as Compress writes none.
        cmd.arg("--no-mac-metadata");
    }
    let landed = run(&mut cmd, task).and_then(|_| {
        let items: Vec<PathBuf> = std::fs::read_dir(&scratch).map_err(|e| crate::i18n::io_text(&e))?.flatten().map(|e| e.path()).collect();
        if let [only] = items.as_slice() {
            let target = free_target(dir, &name_of(only), only.is_dir(), false);
            rename_noreplace(only, &target).map_err(|e| t!("archive.place_failed", name = name_of(only), error = crate::i18n::io_text(&e)))?;
            let _ = std::fs::remove_dir(&scratch);
            Ok(vec![target])
        } else {
            let target = free_target(dir, stem, true, false);
            rename_noreplace(&scratch, &target).map_err(|e| t!("archive.place_failed", name = stem, error = crate::i18n::io_text(&e)))?;
            Ok(vec![target])
        }
    });
    if landed.is_err() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    landed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn listing(archive: &Path) -> Vec<String> {
        let out = Command::new(bsdtar().unwrap()).arg("-tf").arg(archive).output().unwrap();
        let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
        names.sort();
        names
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn archive_names_and_their_stems() {
        assert_eq!(archive_stem("photos.zip"), Some("photos"));
        assert_eq!(archive_stem("backup.tar.gz"), Some("backup"));
        assert_eq!(archive_stem("Backup.TGZ"), Some("Backup"));
        assert_eq!(archive_stem("src.tar.xz"), Some("src"));
        assert_eq!(archive_stem("old.7z"), Some("old"));
        assert_eq!(archive_stem("notes.txt"), None);
        assert_eq!(archive_stem(".zip"), None, "a dotfile is no archive");
    }

    #[test]
    fn compressing_one_item_names_the_archive_after_it() {
        if bsdtar().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        write(&proj.join("a.txt"), "hello");
        write(&proj.join(".DS_Store"), "finder");
        let zip = compress(std::slice::from_ref(&proj), &Task::default()).unwrap();
        assert_eq!(zip, dir.path().join("proj.zip"));
        let inside = listing(&zip);
        assert!(inside.contains(&"proj/a.txt".to_string()), "{inside:?}");
        assert!(inside.iter().all(|n| !n.contains("._") && !n.ends_with(".DS_Store")), "no Mac metadata: {inside:?}");
        assert_eq!(compress(&[proj], &Task::default()).unwrap(), dir.path().join("proj 2.zip"), "the name is taken: keep both");
        assert!(names(dir.path()).iter().all(|n| !n.contains("partial")), "no temporary file left");
    }

    #[test]
    fn compressing_several_items_makes_an_archive() {
        if bsdtar().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), "a");
        write(&dir.path().join("b/c.txt"), "c");
        let zip = compress(&[dir.path().join("a.txt"), dir.path().join("b")], &Task::default()).unwrap();
        assert_eq!(zip, dir.path().join("Archive.zip"));
        let inside = listing(&zip);
        assert!(inside.contains(&"a.txt".to_string()) && inside.contains(&"b/c.txt".to_string()), "{inside:?}");
    }

    #[test]
    fn items_of_different_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("x/a"), "a");
        write(&dir.path().join("y/b"), "b");
        assert!(compress(&[dir.path().join("x/a"), dir.path().join("y/b")], &Task::default()).is_err());
    }

    #[test]
    fn one_item_extracts_beside_the_archive_and_never_merges() {
        if bsdtar().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        write(&proj.join("a.txt"), "hello");
        let zip = compress(std::slice::from_ref(&proj), &Task::default()).unwrap();
        // "proj" is still there: the extracted copy keeps both.
        let landed = extract(&zip, &Task::default()).unwrap();
        assert_eq!(landed, vec![dir.path().join("proj 2")]);
        assert_eq!(fs::read_to_string(dir.path().join("proj 2/a.txt")).unwrap(), "hello");
        assert!(names(dir.path()).iter().all(|n| !n.contains("extracting")), "no temporary folder left");
    }

    #[test]
    fn macos_junk_is_never_extracted() {
        if bsdtar().is_none() {
            return;
        }
        // The drive is mounted with macFUSE's `noappledouble`: creating `.DS_Store` or `._*`
        // there fails, and one such entry made the whole extraction fail.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write(&src.join("kit/f.txt"), "f");
        write(&src.join("kit/.DS_Store"), "finder");
        write(&src.join("kit/._f.txt"), "fork");
        write(&src.join("kit/b/.DS_Store"), "finder");
        write(&src.join("kit/keep.DS_Store.txt"), "kept");
        write(&src.join("__MACOSX/kit/._f.txt"), "fork");
        let zip = dir.path().join("kit.zip");
        let ok = Command::new(bsdtar().unwrap()).args(["-a", "-c", "--no-mac-metadata", "-f"]).arg(&zip).arg("-C").arg(&src).args(["kit", "__MACOSX"]).status().unwrap();
        assert!(ok.success());
        let landed = extract(&zip, &Task::default()).unwrap();
        // `__MACOSX` gone, one item left: it lands under its own name.
        assert_eq!(landed, vec![dir.path().join("kit")]);
        let mut found: Vec<String> = Vec::new();
        for e in walk(&landed[0]) {
            found.push(e.strip_prefix(&landed[0]).unwrap().to_string_lossy().into_owned());
        }
        found.sort();
        assert_eq!(found, ["b", "f.txt", "keep.DS_Store.txt"]);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).unwrap().flatten() {
            out.push(e.path());
            if e.path().is_dir() {
                out.extend(walk(&e.path()));
            }
        }
        out
    }

    #[test]
    fn an_entry_error_is_read_from_its_verbose_line() {
        assert_eq!(
            entry_line("x fontes/.DS_Store: Can't create 'fontes/.DS_Store': No such file or directory"),
            Some(("fontes/.DS_Store", Some("Can't create 'fontes/.DS_Store': No such file or directory")))
        );
        assert_eq!(entry_line("x fontes/Jost/OFL.txt"), Some(("fontes/Jost/OFL.txt", None)));
        assert_eq!(entry_line("a Notes: meeting.txt"), Some(("Notes: meeting.txt", None)), "a colon in a name is no error");
        assert_eq!(entry_line("bsdtar: Error exit delayed from previous errors."), None);
        // The entry's own error is what the window shows, not bsdtar's closing summary.
        let errors = vec!["Can't create 'a/.DS_Store': No such file or directory".to_string(), "Can't create 'b/._x': No such file or directory".to_string()];
        let text = failure_text(&errors, &["bsdtar: Error exit delayed from previous errors.".to_string()]);
        assert!(text.starts_with("Can't create 'a/.DS_Store'"), "{text}");
        assert!(text.contains('1'), "the others are counted: {text}");
        assert_eq!(failure_text(&[], &["bsdtar: Damaged archive".to_string()]), "Damaged archive");
    }

    #[test]
    fn several_items_extract_into_a_folder_named_after_the_archive() {
        if bsdtar().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write(&src.join("a.txt"), "a");
        write(&src.join("b.txt"), "b");
        let tgz = dir.path().join("bundle.tar.gz");
        let ok = Command::new(bsdtar().unwrap()).arg("-czf").arg(&tgz).arg("-C").arg(&src).args(["a.txt", "b.txt"]).status().unwrap();
        assert!(ok.success());
        let landed = extract(&tgz, &Task::default()).unwrap();
        assert_eq!(landed, vec![dir.path().join("bundle")]);
        assert_eq!(names(&dir.path().join("bundle")), ["a.txt", "b.txt"]);
    }

    #[test]
    fn an_entry_that_climbs_out_is_refused_and_nothing_lands() {
        if bsdtar().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        write(&inner.join("evil.txt"), "x");
        let tar = inner.join("evil.tar");
        // An entry named "../evil.txt".
        let ok = Command::new(bsdtar().unwrap())
            .arg("-cf")
            .arg(&tar)
            .arg("-C")
            .arg(&inner)
            .args(["-s", "|^|../|", "evil.txt"])
            .status()
            .unwrap();
        assert!(ok.success());
        fs::remove_file(inner.join("evil.txt")).unwrap();
        assert!(extract(&tar, &Task::default()).is_err());
        assert!(!dir.path().join("evil.txt").exists(), "nothing escaped the folder");
        assert_eq!(names(&inner), ["evil.tar"], "and nothing half extracted stays");
    }

    #[test]
    fn compressing_counts_the_entries_and_a_cancel_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        write(&proj.join("a.txt"), "a");
        write(&proj.join("d/b.txt"), "b");
        let task = Task::default();
        let zip = compress(std::slice::from_ref(&proj), &task).unwrap();
        let s = task.snapshot();
        assert_eq!((s.files_done, s.files_total), (4, 4), "proj, a.txt, d and b.txt");
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::rename(&zip, out.join("p.zip")).unwrap();
        let task = Task::default();
        extract(&out.join("p.zip"), &task).unwrap();
        assert_eq!(task.snapshot().files_done, 4, "entries are counted as they come");

        let cancelled = Task::default();
        cancelled.cancel();
        assert_eq!(compress(std::slice::from_ref(&proj), &cancelled).unwrap_err(), CANCELLED);
        assert_eq!(extract(&out.join("p.zip"), &cancelled).unwrap_err(), CANCELLED);
        let mut left: Vec<String> = fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["p.zip", "proj"], "no partial archive, no scratch folder");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2, "proj and out only");
    }
}
