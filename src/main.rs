use clap::{Arg, ArgMatches, Command};
use drive_sync::config::Config;
use drive_sync::fs::GitDriveFS;
use drive_sync::git;
use drive_sync::log_ts;
use drive_sync::sync::SyncManager;
use fuser::MountOption;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Instala um panic hook que loga o backtrace completo antes do unwind chegar
/// no boundary da sessão FUSE e matar o processo. Sem isso, panics em
/// callbacks FUSE só imprimem `panicked at ...` sem stack — inviabilizando
/// localizar a origem do `Instant + Duration` overflow que mata o daemon
/// durante paste de muitos arquivos no Finder.
fn install_panic_hook() {
    // Forçar backtrace mesmo se o usuário não setou RUST_BACKTRACE no env.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "full") };
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!(
            "[{}] PANIC capturado pelo hook — daemon vai morrer logo em seguida",
            log_ts()
        );
        eprintln!("[{}] PANIC info: {}", log_ts(), info);
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("[{}] PANIC backtrace:\n{}", log_ts(), bt);
        // Delega pro default hook também (mantém comportamento padrão de print).
        default_hook(info);
    }));
}

fn cli() -> Command {
    Command::new("drive-sync")
        .version("0.1.0")
        .about("Sistema de arquivos versionado com Git")
        .subcommand_negates_reqs(true)
        .subcommand(
            Command::new("status")
                .about("Mostra o estado do daemon de sincronização")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .value_name("PATH")
                        .help("Caminho do status.json (padrão: ./logs/status.json, depois o padrão do macOS)"),
                ),
        )
        .subcommand(
            Command::new("install")
                .about("Instala o LaunchAgent: monta o drive automaticamente no login")
                .arg(
                    Arg::new("repo")
                        .short('r')
                        .long("repo")
                        .value_name("REPO_PATH")
                        .required(true)
                        .help("Repositório Git de armazenamento"),
                )
                .arg(
                    Arg::new("mount")
                        .short('m')
                        .long("mount")
                        .value_name("MOUNT_PATH")
                        .required(true)
                        .help("Ponto de montagem do drive"),
                )
                .arg(
                    Arg::new("with-tray")
                        .long("with-tray")
                        .help("Registra também a tray para subir no login")
                        .action(clap::ArgAction::SetTrue),
                ),
        )
        .subcommand(Command::new("uninstall").about("Para o daemon e remove o LaunchAgent"))
        .subcommand(
            Command::new("tray")
                .about("Ícone de status na barra de menus (monitor do daemon)")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .value_name("PATH")
                        .help("Caminho do status.json (padrão: ./logs/status.json, depois o padrão do macOS)"),
                ),
        )
        .arg(
            Arg::new("repo")
                .short('r')
                .long("repo")
                .value_name("REPO_PATH")
                .required(true)
                .help("Caminho para o repositório Git"),
        )
        .arg(
            Arg::new("mount")
                .short('m')
                .long("mount")
                .value_name("MOUNT_PATH")
                .required(true)
                .help("Diretório onde será montado o sistema FUSE"),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Espelha os logs no terminal além de gravar no log dir")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("config")
                .long("config")
                .value_name("PATH")
                .help("Caminho do config.json (padrão: ./config.json)"),
        )
        .arg(
            Arg::new("log-dir")
                .long("log-dir")
                .value_name("DIR")
                .help("Diretório dos logs da sessão (padrão: ./logs)"),
        )
        .arg(
            Arg::new("status-file")
                .long("status-file")
                .value_name("PATH")
                .help("Onde gravar o status.json (padrão: <log-dir>/status.json)"),
        )
}

/// main síncrono: subcomandos utilitários rodam sem runtime; o modo mount cria
/// o runtime tokio explicitamente e conduz o lifecycle (mount, sinais, unmount,
/// drain final). A tray (subcomando futuro) exige a main thread livre de
/// runtime — este dispatch garante isso.
fn main() -> anyhow::Result<()> {
    let matches = cli().get_matches();

    if let Some(sub) = matches.subcommand_matches("status") {
        let file =
            drive_sync::paths::resolve_status_for_read(sub.get_one::<String>("file").map(String::as_str));
        return run_status(&file);
    }
    if let Some(sub) = matches.subcommand_matches("install") {
        return drive_sync::launchd::install(
            sub.get_one::<String>("repo").unwrap(),
            sub.get_one::<String>("mount").unwrap(),
            sub.get_flag("with-tray"),
        );
    }
    if matches.subcommand_matches("uninstall").is_some() {
        return drive_sync::launchd::uninstall();
    }
    if let Some(sub) = matches.subcommand_matches("tray") {
        let file =
            drive_sync::paths::resolve_status_for_read(sub.get_one::<String>("file").map(String::as_str));
        // A tray toma a main thread com o event loop do AppKit — por isso o
        // dispatch acontece antes de qualquer runtime tokio.
        return drive_sync::tray::run(file);
    }

    run_mount(&matches)
}

fn run_mount(matches: &ArgMatches) -> anyhow::Result<()> {
    let repo_path = matches.get_one::<String>("repo").unwrap().clone();
    let mount_path = matches.get_one::<String>("mount").unwrap().clone();
    let verbose = matches.get_flag("verbose");
    let config_path = matches
        .get_one::<String>("config")
        .map(String::as_str)
        .unwrap_or("config.json")
        .to_string();
    let log_dir = matches
        .get_one::<String>("log-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("logs"));
    let status_file = matches
        .get_one::<String>("status-file")
        .map(PathBuf::from)
        .unwrap_or_else(|| log_dir.join("status.json"));

    // Depois do parse dos args (pra `--help`/`--version` ainda saírem no
    // terminal): redirecionar stdout/stderr pro log da sessão. Isso captura
    // nossos println!/eprintln! E o output do crate `log` (fuser via
    // env_logger) num único arquivo. Anuncia o path em `/dev/tty`.
    let log_path = drive_sync::redirect_io_to_log_file(verbose, &log_dir).ok();
    if let Some(ref path) = log_path {
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = writeln!(tty, "log em: {}", path.display());
        }
    }

    // Hook precisa rodar DEPOIS do redirect pra que o stderr caia no arquivo.
    install_panic_hook();

    // env_logger respeita RUST_LOG. Com RUST_LOG=fuser=debug, a gente vê o
    // opcode cru que macFUSE envia.
    env_logger::init();

    let config = Config::load_from(&config_path);
    if let Some(ref path) = log_path {
        println!("[{}] boot: log={}", log_ts(), path.display());
    }
    println!(
        "[{}] boot: debounce={}s",
        log_ts(),
        config.write_debounce_secs
    );
    println!("[{}] boot: repo={}", log_ts(), repo_path);
    println!("[{}] boot: mount={}", log_ts(), mount_path);

    let branch = {
        let repo = git::open_repo(&repo_path)?;
        git::current_branch_name(&repo)?
    };
    println!("[{}] boot: branch={}", log_ts(), branch);

    // Runtime explícito: os watchers do SyncManager precisam do contexto tokio.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _guard = runtime.enter();

    let sync = SyncManager::new_full_with_pull(
        &repo_path,
        branch.clone(),
        config.write_debounce_secs,
        config.user_name.clone(),
        config.user_email.clone(),
        Some("origin".to_string()),
        config.remote_token.clone(),
        config.pull_interval_secs,
    );
    let sync_handle = sync.clone();
    // Chunking de arquivos grandes (Fase 2.6): nenhum blob acima do limiar
    // chega ao commit — vira manifest + .chunks/.
    sync.set_chunking(
        config.chunk_threshold_mb * 1024 * 1024,
        config.chunk_size_mb * 1024 * 1024,
    );
    // Status observável (lido por `drive-sync status` e pela tray)
    sync.enable_status_file(status_file);
    sync.status_state().set_runtime_info(
        &mount_path,
        log_path
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_str(),
    );

    let fs = GitDriveFS::new_with_options(
        &repo_path,
        branch,
        sync,
        config.read_cache_mb * 1024 * 1024,
        config.large_file_threshold_mb * 1024 * 1024,
    )?;

    // Opções macFUSE-específicas:
    // - `local`: trata o volume como disco local em vez de network share. Sem
    //   isso, Finder cacheia listagem e não reage a mudanças que ele mesmo
    //   acabou de fazer (bug conhecido com mounts marcados como "remoto").
    // - `volname=drive-sync`: nome amigável em Finder no lugar de "macFUSE Volume 0".
    // - `noappledouble`: kernel não gera arquivos `._*` (AppleDouble) no mount.
    //   Reforça nosso filtro de ephemeral no sync, mas como fallback nativo.
    let mut options = vec![
        MountOption::FSName("gitdrive".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowOther,
    ];
    #[cfg(target_os = "macos")]
    {
        options.push(MountOption::CUSTOM("local".to_string()));
        options.push(MountOption::CUSTOM("volname=drive-sync".to_string()));
        options.push(MountOption::CUSTOM("noappledouble".to_string()));
        // daemon_timeout default do macFUSE é 60s. Em paste de pasta com muitas
        // sub-pastas/arquivos, o ciclo CREATE+WRITE+FLUSH+RELEASE pode exceder
        // esse tempo se houver pressão de I/O. 300s dá margem.
        options.push(MountOption::CUSTOM("daemon_timeout=300".to_string()));
    }

    // Recuperação pós-crash: kill -9 (ou panic) não roda o AutoUnmount e
    // deixa um mount FUSE zumbi que bloqueia o remount — o KeepAlive do
    // launchd ficaria religando um daemon que falha para sempre. Detecta e
    // força o unmount antes de montar.
    prepare_mountpoint(&mount_path)?;

    // Sessão em background: o main vira o dono do lifecycle e conduz o
    // shutdown ordenado (decisões 6 e 7 da Fase 2).
    let session = fuser::spawn_mount2(fs, &mount_path, &options)?;
    println!("[{}] boot: volume montado em {}", log_ts(), mount_path);
    sync_handle.enable_notifications();
    sync_handle
        .notifier()
        .notify(drive_sync::notify::NotifyEvent::Mounted {
            mount: mount_path.clone(),
        });

    // Espera por SIGTERM/SIGINT (launchd/Ctrl-C) ou pelo fim externo da
    // sessão (eject no Finder / umount).
    let ejected = runtime.block_on(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        loop {
            tokio::select! {
                _ = term.recv() => {
                    println!("[{}] shutdown: SIGTERM recebido", log_ts());
                    return Ok::<bool, anyhow::Error>(false);
                }
                _ = int.recv() => {
                    println!("[{}] shutdown: SIGINT recebido", log_ts());
                    return Ok(false);
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    if session.guard.is_finished() {
                        println!("[{}] shutdown: volume ejetado externamente", log_ts());
                        return Ok(true);
                    }
                }
            }
        }
    })?;

    // Ordem do shutdown (decisão 7): unmount ANTES do drain final — o unmount
    // força o kernel a entregar FLUSH/RELEASE de fds abertos, e essas ops
    // entram na fila a tempo de serem drenadas.
    if !ejected {
        println!("[{}] shutdown: desmontando {}", log_ts(), mount_path);
    }
    session.join();
    sync_handle.drain_now();
    sync_handle.status_state().record_stopped();
    sync_handle
        .notifier()
        .notify(drive_sync::notify::NotifyEvent::Stopped);
    println!("[{}] shutdown: parada limpa concluída", log_ts());

    Ok(())
}

/// Garante que o mountpoint existe e está livre. Se a tabela de mounts já
/// lista o path: sonda com `read_dir` — mount zumbi (crash anterior) falha e
/// leva `umount -f`; mount saudável significa outra instância viva e o boot
/// aborta com erro claro em vez de brigar pelo mountpoint.
fn prepare_mountpoint(mount: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(mount)?;
    let canonical = std::fs::canonicalize(mount)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| mount.to_string());

    let table = std::process::Command::new("mount").output()?;
    let needle = format!(" on {} (", canonical);
    let listed = String::from_utf8_lossy(&table.stdout)
        .lines()
        .any(|l| l.contains(&needle));
    if !listed {
        return Ok(());
    }

    match std::fs::read_dir(&canonical) {
        Ok(_) => anyhow::bail!(
            "já existe um volume saudável montado em {} — pare o outro daemon ou desmonte antes",
            canonical
        ),
        Err(e) => {
            println!(
                "[{}] boot: mount zumbi detectado em {} ({}) — forçando unmount",
                log_ts(),
                canonical,
                e
            );
            let status = std::process::Command::new("umount")
                .arg("-f")
                .arg(&canonical)
                .status()?;
            if !status.success() {
                anyhow::bail!(
                    "não foi possível desmontar o mount zumbi em {} — rode `umount -f {}` manualmente",
                    canonical,
                    canonical
                );
            }
            // Dar um instante para o kernel liberar o mountpoint.
            std::thread::sleep(std::time::Duration::from_millis(300));
            Ok(())
        }
    }
}

/// `drive-sync status`: lê o status.json gravado pelo daemon e formata.
fn run_status(file: &Path) -> anyhow::Result<()> {
    use drive_sync::status;
    match status::read_from(file) {
        Ok(s) => {
            let alive = status::pid_alive(s.pid);
            print!("{}", status::format_status(&s, status::now_unix(), alive));
        }
        Err(e) => {
            println!("Sem status disponível ({}).", e);
            println!(
                "Monte um repositório primeiro: drive-sync -r <repo> -m <mount> — o daemon grava o status.json no log dir (padrão: logs/ do diretório de onde foi lançado). Use --file para apontar outro caminho."
            );
        }
    }
    Ok(())
}
