# git-drive Files (proof of concept)

A read-only file manager with the Control Center's look: glass cards over the indigo gradient,
Liquid Glass on macOS 26, the same accent color (picked in the Control Center) and the same
logo. It browses any folder; the git-drive mount is the first place in the sidebar.

Status: proof of concept on the `poc/file-manager` branch. It does not create, rename, move or
delete anything.

## What it does

| Area | Behavior |
| --- | --- |
| Navigation | Places (the drive, Home, Desktop, Documents, Downloads, Computer), back/forward history, enclosing folder, clickable path (long paths collapse in the middle; the folder in view stays visible). |
| List view | Name, Date Modified, Size, Kind; click a header to sort; folders stay on top; natural order (`file2` before `file10`). |
| Column view | One column per folder from the nearest place down to the folder in view; a selected file shows its preview as the last column. |
| Preview | Images, video, audio and PDF (macOS) through the asset protocol, which reads only the byte ranges a player asks for; text and code (also files without an extension, detected by content) up to 256 KB; anything else shows its details and an Open button. Side pane in list view, last column in column view, full window with Space (Quick Look). |
| Search | By name, case-insensitive, recursive from the folder in view, `.git` skipped, hidden items only when shown. Results stream in batches and are sorted like the list; a new search cancels the running one. Limits: 1,000 results, 200,000 entries visited. |
| Other | Show or hide hidden files (dot-files and macOS hidden flags), open in the default app, reveal in the system file manager, free space of the volume (inside the drive, the repository's volume). |

## Keyboard

| Keys | Action |
| --- | --- |
| Up / Down | Move the selection |
| Left / Right | Column view: parent column / into the selected folder |
| Enter, Cmd+Down | Open the folder or file |
| Cmd+Up | Enclosing folder |
| Cmd+[ / Cmd+] | Back / forward |
| Space | Quick Look |
| Cmd+F, Esc | Search, clear the search |
| Cmd+2 / Cmd+3 | List view / column view |
| Cmd+Shift+. | Show or hide hidden files |
| Cmd+R | Reload the folders in view |

## Build and run

```bash
cd file-manager
npm install
npm run tauri build -- --bundles app    # macOS; on Linux: npm run tauri build -- --no-bundle
../target/release/git-drive-files                  # opens at the drive (or Home)
../target/release/git-drive-files --path ~/Pictures
cargo test --release -p git-drive-files            # file model tests
```

Needs Node 20+ and, on Linux, `webkit2gtk-4.1`, like the Control Center. The crate is a
workspace member outside the default members, so root `cargo` commands never build it.

## How it is built

| Part | Role |
| --- | --- |
| `src-tauri/src/fsmodel.rs` | The filesystem model, tested without a webview: `list_dir` (natural order, folders first, hidden detection), `classify` (preview kind by extension, then content), `read_text` (bounded, cut on a character boundary), `search` (breadth-first, cancellable, reports each match), `places`. |
| `src-tauri/src/main.rs` | Tauri commands over the model (`boot`, `list_dir`, `preview_kind`, `read_text`, `search` with a result `Channel`, `volume`, `open_path`), the window and its glass. |
| `src/lib/` | Typed commands (`api.ts`), navigation state shared by both views (`nav.ts`), folder listings cache (`useDir.ts`), kinds, icons and dates (`kinds.ts`), row scrolling that never moves other containers (`scroll.ts`). |
| `src/views/`, `src/components/` | List view, column view, preview pane and Quick Look; sidebar and toolbar. |
| `src/ui/` | The Control Center's design system, copied (styles, accent, format, UI pieces, logo). Keep it in sync with `control-center/src` until both apps share a package. |

## Known limitations

- PDF preview needs the macOS webview; on Linux a PDF shows its details and an Open button.
- Search matches names only and has no index; a first search over a large drive takes seconds
  (results appear as they are found).
- The asset protocol may read any file of the user (scope `**`): acceptable for a local POC,
  to be narrowed before it ships.
- The git-drive FUSE mount reports placeholder free-space numbers (no `statfs`), so the drive's
  free space is measured on the repository's volume; Finder shows the placeholder.
- On Linux, WebKitGTK's helper process sometimes crashes while the window closes (as in the
  Control Center); no effect on the app.
