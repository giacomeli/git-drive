//! The terminal panel's shells: each tab is a pseudo-terminal running the
//! user's login shell, its output streamed to the window as raw bytes (the
//! webview's xterm.js decodes them), its input and size coming back from it.
//! A tab's title and its relative links follow the shell's working folder,
//! read from the system (`/proc` on Linux, `proc_pidinfo` on macOS).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// A terminal's size in character cells.
#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl From<Size> for PtySize {
    fn from(s: Size) -> Self {
        PtySize { rows: s.rows.max(1), cols: s.cols.max(1), pixel_width: 0, pixel_height: 0 }
    }
}

/// The user's shell (`$SHELL` when it is an absolute path, else the
/// system's default) as a login shell: the same environment and PATH as a
/// terminal opened from scratch (an app launched from Finder has almost none).
pub fn shell_for(shell_env: Option<&str>, macos: bool) -> (String, Vec<String>) {
    let shell = shell_env
        .filter(|s| s.starts_with('/'))
        .map_or_else(|| if macos { "/bin/zsh" } else { "/bin/bash" }.to_string(), str::to_string);
    (shell, vec!["-l".to_string()])
}

/// A path as a shell reads it: as is when it holds only safe characters,
/// else in single quotes (a quote inside becomes `'\''`).
pub fn quote_for_shell(path: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+=,:@%".contains(c);
    if !path.is_empty() && path.chars().all(safe) {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// What a drop of `paths` types into a terminal: each path quoted, a space after each.
pub fn paste_text(paths: &[String]) -> String {
    paths.iter().map(|p| quote_for_shell(p) + " ").collect()
}

/// What the panel's "go to the folder in view" button types: a `cd` and Return.
pub fn cd_text(path: &str) -> String {
    format!("cd {}\r", quote_for_shell(path))
}

/// The working folder of process `pid`.
#[cfg(target_os = "linux")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The working folder of process `pid`.
#[cfg(target_os = "macos")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the buffer is a zeroed proc_vnodepathinfo of the size given.
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDVNODEPATHINFO, 0, (&mut info as *mut libc::proc_vnodepathinfo).cast(), size) };
    if n != size {
        return None;
    }
    // SAFETY: vip_path is a NUL-terminated path in a MAXPATHLEN buffer.
    let path = unsafe { std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    (!path.to_bytes().is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

/// The command line of process `pid` (its arguments, the program first).
#[cfg(target_os = "linux")]
pub fn process_args(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> = raw.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    (!args.is_empty()).then_some(args)
}

/// The command line of process `pid` (its arguments, the program first),
/// from `KERN_PROCARGS2`: argc, the executable's path, padding, then argv.
#[cfg(target_os = "macos")]
pub fn process_args(pid: u32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query (null buffer), then a read into a buffer of that size.
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        buf.truncate(size);
        let argc = i32::from_ne_bytes(buf.get(0..4)?.try_into().ok()?) as usize;
        let rest = &buf[4..];
        // The executable's path, then NULs up to the first argument.
        let path_end = rest.iter().position(|&b| b == 0)?;
        let start = path_end + rest[path_end..].iter().position(|&b| b != 0)?;
        let args: Vec<String> = rest[start..].split(|&b| b == 0).take(argc).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        (!args.is_empty()).then_some(args)
    }
}

/// The server an `ssh` command line connects to, as typed (`maestro`,
/// `user@host`), with `-l user` folded in; None for anything else.
pub fn ssh_destination(args: &[String]) -> Option<String> {
    let program = Path::new(args.first()?).file_name()?.to_string_lossy().into_owned();
    if program != "ssh" {
        return None;
    }
    // Options that take a value (ssh(1)).
    const VALUED: &str = "BbcDEeFIiJLlmOoPpQRSWw";
    let mut login: Option<String> = None;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        if a == "--" {
            return it.next().map(|d| with_login(d, login.as_deref()));
        }
        let Some(flags) = a.strip_prefix('-').filter(|f| !f.is_empty()) else {
            return Some(with_login(a, login.as_deref()));
        };
        for (i, c) in flags.char_indices() {
            if VALUED.contains(c) {
                let value = if i + c.len_utf8() < flags.len() { Some(flags[i + c.len_utf8()..].to_string()) } else { it.next().cloned() };
                if c == 'l' {
                    login = value;
                }
                break;
            }
        }
    }
    None
}

fn with_login(dest: &str, login: Option<&str>) -> String {
    let d = dest.strip_prefix("ssh://").unwrap_or(dest);
    // ssh://user@host:2222 -> user@host
    let d = if dest.starts_with("ssh://") { d.rsplit_once(':').map_or(d, |(h, _)| h) } else { d };
    match login {
        Some(l) if !d.contains('@') => format!("{l}@{d}"),
        _ => d.to_string(),
    }
}

/// One tab's shell on its pseudo-terminal. Dropping it ends the shell.
pub struct Session {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
}

impl Session {
    /// Starts `program` in `cwd` on a new pseudo-terminal. `on_data` gets
    /// the output as it comes (raw bytes, possibly cut inside a character);
    /// `on_exit` the exit code once the program ends and its output is read.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        size: Size,
        mut on_data: impl FnMut(&[u8]) + Send + 'static,
        on_exit: impl FnOnce(Option<u32>) + Send + 'static,
    ) -> Result<Session, String> {
        let pair = native_pty_system().openpty(size.into()).map_err(|e| t!("terminal.open_failed", error = e))?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "git-drive-files");
        // An app opened from Finder has no locale: without one, zsh and
        // readline treat accented names as bytes.
        if std::env::var_os("LANG").is_none() && std::env::var_os("LC_ALL").is_none() {
            cmd.env("LANG", "en_US.UTF-8");
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| t!("terminal.start_failed", program = program, error = e))?;
        // Only the child keeps the terminal's other end: its exit ends the output.
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        let (read_done, read_finished) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => on_data(&buf[..n]),
                }
            }
            let _ = read_done.send(());
        });
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code());
            // The last output first; a background job that keeps the
            // terminal open must not hold the exit back for long.
            let _ = read_finished.recv_timeout(Duration::from_millis(300));
            on_exit(code);
        });
        Ok(Session { master: Mutex::new(pair.master), writer: Mutex::new(writer), killer: Mutex::new(killer), pid })
    }

    /// Types `bytes` into the terminal (keys, pasted text).
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        let mut w = self.writer.lock().map_err(|_| t!("terminal.gone"))?;
        w.write_all(bytes).and_then(|_| w.flush()).map_err(|e| crate::i18n::io_text(&e))
    }

    pub fn resize(&self, size: Size) -> Result<(), String> {
        self.master.lock().map_err(|_| t!("terminal.gone"))?.resize(size.into()).map_err(|e| e.to_string())
    }

    /// The shell's working folder now.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.pid.and_then(process_cwd)
    }

    /// The command line of what runs in front of the shell (the leader of
    /// the terminal's foreground process group); None at the prompt.
    pub fn foreground(&self) -> Option<Vec<String>> {
        let leader = self.master.lock().ok()?.process_group_leader()?;
        let leader = u32::try_from(leader).ok()?;
        if Some(leader) == self.pid {
            return None;
        }
        process_args(leader)
    }

    /// Whether the shell itself has the terminal (it leads the foreground
    /// process group): no command runs in front, so a key sent now is the
    /// shell's own, never typed into a program.
    pub fn at_prompt(&self) -> bool {
        let Ok(master) = self.master.lock() else { return false };
        matches!((master.process_group_leader(), self.pid), (Some(leader), Some(pid)) if u32::try_from(leader) == Ok(pid))
    }

    /// After a change of width: the shell redraws its screen (Ctrl+L) when it
    /// is at its prompt. Lines of an old prompt (a right prompt above all)
    /// wrap when the width changes, and the shell's own redraw on SIGWINCH
    /// leaves copies of a two-line prompt; a clear screen puts what was there
    /// in the scrollback (the window sets `scrollOnEraseInDisplay`) and draws
    /// one clean prompt. `wait` gives the shell time to take the new size first.
    pub fn redraw_at_prompt(self: &std::sync::Arc<Self>, wait: Duration) {
        let session = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            if session.at_prompt() {
                let _ = session.write(b"\x0c");
            }
        });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn the_shell_is_the_users_own_as_a_login_shell() {
        assert_eq!(shell_for(Some("/usr/local/bin/fish"), true), ("/usr/local/bin/fish".to_string(), vec!["-l".to_string()]));
        assert_eq!(shell_for(None, true).0, "/bin/zsh", "macOS's default");
        assert_eq!(shell_for(Some(""), false).0, "/bin/bash", "Linux's default");
        assert_eq!(shell_for(Some("fish"), false).0, "/bin/bash", "only an absolute path is taken");
    }

    #[test]
    fn dropped_paths_are_quoted_only_when_needed() {
        assert_eq!(quote_for_shell("/Users/u/notes.md"), "/Users/u/notes.md");
        assert_eq!(quote_for_shell("/Users/u/My Files/a b.txt"), "'/Users/u/My Files/a b.txt'");
        assert_eq!(quote_for_shell("/tmp/it's"), r"'/tmp/it'\''s'");
        assert_eq!(quote_for_shell("/tmp/$HOME*"), "'/tmp/$HOME*'");
        assert_eq!(paste_text(&["/a b".into(), "/c".into()]), "'/a b' /c ");
        assert_eq!(cd_text("/Users/u/My Drive"), "cd '/Users/u/My Drive'\r");
    }

    #[test]
    fn a_session_streams_output_and_reports_the_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (done_tx, done_rx) = mpsc::channel::<Option<u32>>();
        let session = Session::spawn(
            "/bin/sh",
            &["-c".into(), "printf 'hello from %s' \"$(basename \"$PWD\")\"; exit 3".into()],
            dir.path(),
            Size { cols: 80, rows: 24 },
            move |bytes| {
                let _ = tx.send(bytes.to_vec());
            },
            move |code| {
                let _ = done_tx.send(code);
            },
        )
        .unwrap();
        let code = done_rx.recv_timeout(Duration::from_secs(10)).expect("the shell ends");
        let out: Vec<u8> = rx.try_iter().flatten().collect();
        let text = String::from_utf8_lossy(&out);
        let name = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(text.contains(&format!("hello from {name}")), "{text:?}");
        assert_eq!(code, Some(3));
        drop(session);
    }

    #[test]
    fn input_and_size_reach_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (done_tx, done_rx) = mpsc::channel::<Option<u32>>();
        let session = Session::spawn(
            "/bin/sh",
            &["-c".into(), "read line; stty size; echo \"got $line\"".into()],
            dir.path(),
            Size { cols: 80, rows: 24 },
            move |bytes| {
                let _ = tx.send(bytes.to_vec());
            },
            move |code| {
                let _ = done_tx.send(code);
            },
        )
        .unwrap();
        session.resize(Size { cols: 101, rows: 33 }).unwrap();
        session.write(b"ping\n").unwrap();
        done_rx.recv_timeout(Duration::from_secs(10)).expect("the shell ends");
        let text = String::from_utf8_lossy(&rx.try_iter().flatten().collect::<Vec<u8>>()).into_owned();
        assert!(text.contains("33 101"), "the new size: {text:?}");
        assert!(text.contains("got ping"), "{text:?}");
    }

    #[test]
    fn the_ssh_destination_is_read_from_its_command_line() {
        let v = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(ssh_destination(&v("ssh maestro")).as_deref(), Some("maestro"));
        assert_eq!(ssh_destination(&v("/usr/bin/ssh -p 2222 -i key.pem deploy@10.0.0.4 uptime")).as_deref(), Some("deploy@10.0.0.4"));
        assert_eq!(ssh_destination(&v("ssh -vA -oStrictHostKeyChecking=no -l root box")).as_deref(), Some("root@box"));
        assert_eq!(ssh_destination(&v("ssh -J bastion -- prod")).as_deref(), Some("prod"));
        assert_eq!(ssh_destination(&v("ssh -p2222 ssh://ana@host:2200")).as_deref(), Some("ana@host"));
        assert_eq!(ssh_destination(&v("ssh -V")), None);
        assert_eq!(ssh_destination(&v("vim notes.md")), None);
    }

    #[test]
    fn a_process_command_line_is_read_from_the_system() {
        let mut child = std::process::Command::new("/bin/sleep").arg("7").spawn().unwrap();
        // Right after the spawn the child may still be in exec (Linux then reads an empty command line).
        let args = (0..100)
            .find_map(|_| {
                let a = process_args(child.id()).filter(|a| a[0].ends_with("sleep"));
                if a.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                a
            })
            .expect("readable");
        child.kill().unwrap();
        let _ = child.wait();
        assert_eq!(args.len(), 2, "{args:?}");
        assert!(args[0].ends_with("sleep") && args[1] == "7", "{args:?}");
    }

    #[test]
    fn the_prompt_is_told_from_a_command_running_in_front() {
        let dir = tempfile::tempdir().unwrap();
        let (done_tx, _done_rx) = mpsc::channel::<Option<u32>>();
        // An interactive shell: job control puts each command in a process group of its own.
        let session = Session::spawn(
            "/bin/sh",
            &["-i".into()],
            dir.path(),
            Size { cols: 80, rows: 24 },
            |_| {},
            move |code| {
                let _ = done_tx.send(code);
            },
        )
        .unwrap();
        let waited = |want: bool| (0..60).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            session.at_prompt() == want
        });
        assert!(waited(true), "the shell at its prompt");
        assert_eq!(session.foreground(), None);
        session.write(b"sleep 5\n").unwrap();
        assert!(waited(false), "sleep in front");
        let front = session.foreground().expect("a command in front");
        assert!(front[0].ends_with("sleep") && front.get(1).map(String::as_str) == Some("5"), "{front:?}");
        drop(session);
    }

    #[test]
    fn the_working_folder_of_a_process_is_read_from_the_system() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("/bin/sleep").arg("5").current_dir(dir.path()).spawn().unwrap();
        let cwd = process_cwd(child.id()).expect("readable");
        assert_eq!(cwd.canonicalize().unwrap(), dir.path().canonicalize().unwrap());
        child.kill().unwrap();
        let _ = child.wait();
    }
}
