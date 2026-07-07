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

use std::time::{SystemTime, UNIX_EPOCH};

fn local_tm(epoch: i64) -> libc::tm {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&epoch as *const i64 as *const libc::time_t, &mut tm);
    }
    tm
}

/// Local timestamp (HH:MM:SS.mmm) used as a log prefix. No external crates
/// involved — computed straight from `SystemTime::now()`. We adjust to the
/// local offset (from `localtime_r`) instead of UTC so it matches the clock
/// the user sees in the terminal.
pub fn log_ts() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let tm = local_tm(dur.as_secs() as i64);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        dur.subsec_millis()
    )
}

/// Timestamp in `YYYYMMDD-HHMMSS` format for file names.
fn filename_ts() -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let tm = local_tm(epoch);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Redirects the process's stdout and stderr to a file in `logs/` named
/// after the boot timestamp. Returns the path of the created file (printed
/// to the original output before the redirect, so the user knows where to
/// look).
///
/// When `verbose` is `true`, tees: writes simultaneously to the file AND to
/// the original terminal. Implemented via an internal pipe + a thread that
/// reads from the read-end and forwards to both destinations. In non-verbose
/// mode, does a `dup2` straight to the file (lighter, no extra thread).
///
/// In both modes, this captures both our own `println!`/`eprintln!` and the
/// output of the `log` crate (via env_logger) from fuser and other libs, all
/// in the same place.
///
/// `println!` on macOS switches to block buffering when stdout is not a tty,
/// so we disable buffering on the write handle immediately.
pub fn redirect_io_to_log_file(
    verbose: bool,
    log_dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let dir = log_dir.to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("drive-sync-{}.log", filename_ts()));

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;

    // Flush before touching fds — any pending print goes to the old
    // destination (the boot terminal) instead of partially landing in the
    // new redirect.
    let _ = std::io::Write::flush(&mut std::io::stdout().lock());
    let _ = std::io::Write::flush(&mut std::io::stderr().lock());

    if verbose {
        spawn_tee_redirect(file)?;
    } else {
        use std::os::unix::io::IntoRawFd;
        let fd = file.into_raw_fd();
        unsafe {
            if libc::dup2(fd, libc::STDOUT_FILENO) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(fd, libc::STDERR_FILENO) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(fd);
        }
    }

    // In both modes: turn off block buffering on stdout. `setvbuf(_IOLBF)`
    // makes it line-by-line — needed for `tail -f` and for the tee thread to
    // wake up right after each println.
    unsafe {
        let stdout_stream = libc::fdopen(libc::STDOUT_FILENO, c"w".as_ptr());
        if !stdout_stream.is_null() {
            libc::setvbuf(stdout_stream, std::ptr::null_mut(), libc::_IOLBF, 0);
        }
    }

    Ok(path)
}

/// Implements the verbose tee: creates a pipe, dup2s the write-end onto
/// STDOUT/STDERR, and spawns a thread that reads from the read-end and
/// replicates into `file` + the terminal's original fds (preserved via `dup`
/// before the destructive dup2).
fn spawn_tee_redirect(file: std::fs::File) -> std::io::Result<()> {
    let term_stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
    let term_stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
    if term_stdout < 0 || term_stderr < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut pipe_fds: [i32; 2] = [0, 0];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    std::thread::spawn(move || {
        use std::io::Write;
        let mut file = file;
        let mut buf = [0u8; 4096];
        loop {
            let n =
                unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            let slice = &buf[..n as usize];
            let _ = file.write_all(slice);
            let _ = file.flush();
            // Write directly to the preserved terminal fd — it does not go
            // through STDOUT_FILENO (which now points to the pipe). Errors
            // are ignored; if the terminal goes away (tty closed), the
            // thread keeps writing to the file.
            unsafe {
                libc::write(
                    term_stdout,
                    slice.as_ptr() as *const libc::c_void,
                    slice.len() as libc::size_t,
                );
            }
        }
        unsafe {
            libc::close(read_fd);
            libc::close(term_stdout);
            libc::close(term_stderr);
        }
    });

    unsafe {
        if libc::dup2(write_fd, libc::STDOUT_FILENO) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::dup2(write_fd, libc::STDERR_FILENO) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::close(write_fd);
    }

    Ok(())
}
