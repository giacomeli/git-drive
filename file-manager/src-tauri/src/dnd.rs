//! Drag and drop: whether a drop moves or copies, by the rule Finder and
//! Files follow. The drag itself, out of the window, is `native_drag`.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DropOp {
    Move,
    Copy,
}

/// Keys held at the drop that force an operation: Option copies and Cmd
/// moves on macOS, Ctrl copies and Shift moves on Linux.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Forced {
    pub copy: bool,
    pub moves: bool,
}

/// A held key decides; otherwise items move within a volume and are copied to
/// another one. Copy wins over move: it never loses the original.
pub fn drop_op(forced: Forced, same_volume: bool) -> DropOp {
    if forced.copy || !(forced.moves || same_volume) {
        DropOp::Copy
    } else {
        DropOp::Move
    }
}

/// Whether `item` (a link itself, not what it points to) lives on the volume
/// of the folder `dest`.
pub fn same_volume(item: &Path, dest: &Path) -> bool {
    match (fs::symlink_metadata(item), fs::metadata(dest)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// What dropping `sources` on the folder `dest` does. They travel as one drag,
/// so the first item's volume decides for all, as in Finder.
pub fn drop_operation(sources: &[PathBuf], dest: &Path, forced: Forced) -> DropOp {
    drop_op(forced, sources.first().is_some_and(|s| same_volume(s, dest)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn a_drop_moves_within_a_volume_and_copies_to_another() {
        assert_eq!(drop_op(Forced::default(), true), DropOp::Move);
        assert_eq!(drop_op(Forced::default(), false), DropOp::Copy);
    }

    #[test]
    fn a_held_key_forces_the_operation() {
        let copy = Forced { copy: true, moves: false };
        let moves = Forced { copy: false, moves: true };
        assert_eq!(drop_op(copy, true), DropOp::Copy, "Option (Ctrl) copies within a volume");
        assert_eq!(drop_op(moves, false), DropOp::Move, "Cmd (Shift) moves to another volume");
        assert_eq!(drop_op(Forced { copy: true, moves: true }, true), DropOp::Copy, "copying never loses the original");
    }

    #[test]
    fn volumes_are_told_apart_by_device() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        fs::create_dir(dir.path().join("dest")).unwrap();
        assert!(same_volume(&file, &dir.path().join("dest")));
        // /dev is its own file system on macOS (devfs) and Linux (devtmpfs).
        assert!(!same_volume(&file, Path::new("/dev")));
        assert!(!same_volume(&dir.path().join("gone"), &dir.path().join("dest")), "a missing item is not on the volume");
    }

    #[test]
    fn the_first_item_decides_for_the_whole_drag() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(drop_operation(std::slice::from_ref(&file), dir.path(), Forced::default()), DropOp::Move);
        assert_eq!(drop_operation(&[file], Path::new("/dev"), Forced::default()), DropOp::Copy);
        assert_eq!(drop_operation(&[], dir.path(), Forced::default()), DropOp::Copy, "nothing to tell: never move");
    }
}
