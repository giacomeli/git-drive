pub mod config;
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

/// Timestamp local (HH:MM:SS.mmm) usado como prefixo de logs. Não depende de
/// crates externos — calcula direto do `SystemTime::now()`. A gente ajusta pro
/// offset local (de `localtime_r`) ao invés de UTC pra casar com o relógio
/// que o usuário vê no terminal.
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

/// Timestamp no formato `YYYYMMDD-HHMMSS` para nomes de arquivo.
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

/// Redireciona stdout e stderr do processo para um arquivo em `logs/` nomeado
/// com o timestamp de boot. Retorna o path do arquivo criado (printado na
/// saída original antes do redirect, pra que o usuário saiba onde olhar).
///
/// Quando `verbose` é `true`, faz tee: escreve simultaneamente no arquivo E
/// no terminal original. Implementado via pipe interno + thread que lê do
/// read-end e propaga pra ambos os destinos. Em modo não-verbose, faz `dup2`
/// direto pra arquivo (mais leve, sem thread extra).
///
/// Em ambos os modos, captura tanto `println!`/`eprintln!` nossos quanto o
/// output do crate `log` (via env_logger) do fuser e outras libs, tudo no
/// mesmo lugar.
///
/// `println!` em macOS passa a usar buffer de bloco quando stdout não é tty,
/// então a gente desativa o buffer do handle de escrita imediatamente.
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

    // Flush antes de mexer com fds — qualquer print pendente vai pro destino
    // antigo (terminal de boot) em vez de cair no novo redirect parcialmente.
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

    // Em ambos os modos: desliga buffering de bloco em stdout. `setvbuf(_IOLBF)`
    // faz linha-a-linha — necessário pra `tail -f` e pro tee thread acordar
    // logo após cada println.
    unsafe {
        let stdout_stream = libc::fdopen(libc::STDOUT_FILENO, c"w".as_ptr());
        if !stdout_stream.is_null() {
            libc::setvbuf(stdout_stream, std::ptr::null_mut(), libc::_IOLBF, 0);
        }
    }

    Ok(path)
}

/// Implementa o tee verbose: cria pipe, dup2 do write-end pra STDOUT/STDERR e
/// uma thread que lê do read-end e replica no `file` + nos fds originais do
/// terminal (preservados via `dup` antes do dup2 destrutivo).
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
            // Escrita direta no fd preservado do terminal — não passa por
            // STDOUT_FILENO (que agora aponta pro pipe). Erros ignorados;
            // se o terminal sumir (tty fechado), a thread continua escrevendo
            // no arquivo.
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
