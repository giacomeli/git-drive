use clap::{Arg, ArgMatches, Command};
use drive_sync::config::Config;
use drive_sync::fs::GitDriveFS;
use drive_sync::git;
use drive_sync::log_ts;
use drive_sync::sync::SyncManager;
use fuser::MountOption;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Installs a panic hook that logs the full backtrace before the unwind
/// reaches the FUSE session boundary and kills the process. Without it,
/// panics in FUSE callbacks only print `panicked at ...` with no stack —
/// making it impossible to locate the origin of the `Instant + Duration`
/// overflow that kills the daemon while pasting many files in Finder.
fn install_panic_hook() {
    // Force a backtrace even if the user did not set RUST_BACKTRACE in the env.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "full") };
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!(
            "[{}] PANIC caught by the hook — the daemon is about to die",
            log_ts()
        );
        eprintln!("[{}] PANIC info: {}", log_ts(), info);
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("[{}] PANIC backtrace:\n{}", log_ts(), bt);
        // Also delegate to the default hook (keeps the standard print behavior).
        default_hook(info);
    }));
}

fn cli() -> Command {
    Command::new("drive-sync")
        .version("0.1.0")
        .about("Git-backed versioned filesystem")
        .subcommand_negates_reqs(true)
        .subcommand(
            Command::new("status")
                .about("Show the sync daemon state")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .value_name("PATH")
                        .help("Path to status.json (default: ./logs/status.json, then the macOS default)"),
                ),
        )
        .subcommand(
            Command::new("install")
                .about("Install the LaunchAgent: mounts the drive automatically at login")
                .arg(
                    Arg::new("repo")
                        .short('r')
                        .long("repo")
                        .value_name("REPO_PATH")
                        .required(true)
                        .help("Storage Git repository"),
                )
                .arg(
                    Arg::new("mount")
                        .short('m')
                        .long("mount")
                        .value_name("MOUNT_PATH")
                        .required(true)
                        .help("Drive mount point"),
                )
                .arg(
                    Arg::new("with-tray")
                        .long("with-tray")
                        .help("Also register the tray to start at login")
                        .action(clap::ArgAction::SetTrue),
                ),
        )
        .subcommand(Command::new("uninstall").about("Stop the daemon and remove the LaunchAgent"))
        .subcommand(
            Command::new("key")
                .about("Manage the at-rest encryption key")
                .subcommand(
                    Command::new("generate")
                        .about("Generate a new key file (64 hex, permission 0600)")
                        .arg(
                            Arg::new("out")
                                .long("out")
                                .value_name("PATH")
                                .help("Where to write the key (default: ~/.config/drive-sync/drive.key)"),
                        )
                        .arg(
                            Arg::new("force")
                                .long("force")
                                .help("Overwrite an existing key (the old one is LOST — data encrypted with it becomes unreadable)")
                                .action(clap::ArgAction::SetTrue),
                        ),
                ),
        )
        .subcommand(
            Command::new("tray")
                .about("Status icon in the menu bar (daemon monitor)")
                .arg(
                    Arg::new("file")
                        .long("file")
                        .value_name("PATH")
                        .help("Path to status.json (default: ./logs/status.json, then the macOS default)"),
                ),
        )
        .arg(
            Arg::new("repo")
                .short('r')
                .long("repo")
                .value_name("REPO_PATH")
                .required(true)
                .help("Path to the Git repository"),
        )
        .arg(
            Arg::new("mount")
                .short('m')
                .long("mount")
                .value_name("MOUNT_PATH")
                .required(true)
                .help("Directory where the FUSE filesystem will be mounted"),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Mirror logs to the terminal in addition to writing to the log dir")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("config")
                .long("config")
                .value_name("PATH")
                .help("Path to config.json (default: ./config.json, then ~/.config/drive-sync/config.json)"),
        )
        .arg(
            Arg::new("log-dir")
                .long("log-dir")
                .value_name("DIR")
                .help("Session log directory (default: ./logs)"),
        )
        .arg(
            Arg::new("status-file")
                .long("status-file")
                .value_name("PATH")
                .help("Where to write status.json (default: <log-dir>/status.json)"),
        )
}

/// Synchronous main: utility subcommands run without a runtime; mount mode
/// creates the tokio runtime explicitly and drives the lifecycle (mount,
/// signals, unmount, final drain). The tray requires the main thread to be
/// free of any runtime — this dispatch guarantees that.
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
    if let Some(sub) = matches.subcommand_matches("key") {
        if let Some(gen) = sub.subcommand_matches("generate") {
            let out = gen
                .get_one::<String>("out")
                .map(PathBuf::from)
                .unwrap_or_else(drive_sync::paths::standard_key_path);
            drive_sync::crypt::generate_key_file(&out, gen.get_flag("force"))?;
            println!("key generated at: {}", out.display());
            println!("IMPORTANT: without this key the encrypted data is UNRECOVERABLE — back it up outside the repo.");
            return Ok(());
        }
        anyhow::bail!("usage: drive-sync key generate [--out <path>] [--force]");
    }
    if let Some(sub) = matches.subcommand_matches("tray") {
        let file =
            drive_sync::paths::resolve_status_for_read(sub.get_one::<String>("file").map(String::as_str));
        // The tray takes over the main thread with the AppKit event loop —
        // that is why the dispatch happens before any tokio runtime.
        return drive_sync::tray::run(file);
    }

    run_mount(&matches)
}

fn run_mount(matches: &ArgMatches) -> anyhow::Result<()> {
    let repo_path = matches.get_one::<String>("repo").unwrap().clone();
    let mount_path = matches.get_one::<String>("mount").unwrap().clone();
    let verbose = matches.get_flag("verbose");
    let config_path = drive_sync::paths::resolve_config_for_read(
        matches.get_one::<String>("config").map(String::as_str),
    )
    .to_string_lossy()
    .into_owned();
    let log_dir = matches
        .get_one::<String>("log-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("logs"));
    let status_file = matches
        .get_one::<String>("status-file")
        .map(PathBuf::from)
        .unwrap_or_else(|| log_dir.join("status.json"));

    // After arg parsing (so `--help`/`--version` still print to the
    // terminal): redirect stdout/stderr to the session log. This captures
    // our println!/eprintln! AND the `log` crate output (fuser via
    // env_logger) in a single file. Announces the path on `/dev/tty`.
    let log_path = drive_sync::redirect_io_to_log_file(verbose, &log_dir).ok();
    if let Some(ref path) = log_path {
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = writeln!(tty, "log at: {}", path.display());
        }
    }

    // The hook must run AFTER the redirect so stderr lands in the file.
    install_panic_hook();

    // env_logger honors RUST_LOG. With RUST_LOG=fuser=debug, we can see the
    // raw opcode macFUSE sends.
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

    // Explicit runtime: the SyncManager watchers need the tokio context.
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
    // At-rest encryption (Phase 2.7): enabled without a readable key ABORTS
    // the boot — degrading to cleartext would publish plaintext while
    // believing it encrypts.
    let crypto: Option<std::sync::Arc<drive_sync::crypt::Crypto>> = if config.encryption_enabled {
        let key_path =
            drive_sync::paths::resolve_key_for_read(config.encryption_key_file.as_deref());
        let master = drive_sync::crypt::load_key_file(&key_path).map_err(|e| {
            anyhow::anyhow!(
                "encryption enabled in config but the key could not be loaded: {} — generate one with `drive-sync key generate`",
                e
            )
        })?;
        println!(
            "[{}] boot: encryption ENABLED (key: {})",
            log_ts(),
            key_path.display()
        );
        Some(std::sync::Arc::new(
            drive_sync::crypt::Crypto::from_master_key(&master),
        ))
    } else {
        // Disabled in config: if the key exists, it stays available ONLY for
        // the migration back to cleartext (the worktree may still be encrypted).
        let key_path =
            drive_sync::paths::resolve_key_for_read(config.encryption_key_file.as_deref());
        if key_path.exists() {
            if let Ok(master) = drive_sync::crypt::load_key_file(&key_path) {
                sync.set_migration_crypto(Some(std::sync::Arc::new(
                    drive_sync::crypt::Crypto::from_master_key(&master),
                )));
                println!(
                    "[{}] boot: encryption disabled; key present — migration to cleartext active",
                    log_ts()
                );
            }
        }
        None
    };
    sync.set_crypto(crypto.clone());
    // Large-file chunking (Phase 2.6): no blob above the threshold reaches
    // the commit — it becomes a manifest + .chunks/.
    sync.set_chunking(
        config.chunk_threshold_mb * 1024 * 1024,
        config.chunk_size_mb * 1024 * 1024,
    );
    // Observable status (read by `drive-sync status` and by the tray)
    sync.enable_status_file(status_file);
    sync.status_state().set_runtime_info(
        &mount_path,
        log_path
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_str(),
    );

    let fs = GitDriveFS::new_with_options_crypto(
        &repo_path,
        branch,
        sync,
        config.read_cache_mb * 1024 * 1024,
        config.large_file_threshold_mb * 1024 * 1024,
        crypto,
    )?;

    // macFUSE-specific options:
    // - `local`: treats the volume as a local disk instead of a network share.
    //   Without it, Finder caches listings and does not react to changes it
    //   just made itself (known bug with mounts flagged as "remote").
    // - `volname=drive-sync`: friendly name in Finder instead of "macFUSE Volume 0".
    // - `noappledouble`: the kernel does not generate `._*` (AppleDouble) files
    //   on the mount. Reinforces our ephemeral filter in sync, as a native fallback.
    #[allow(unused_mut)]
    let mut options = vec![MountOption::FSName("gitdrive".to_string())];
    #[cfg(target_os = "macos")]
    {
        // AllowOther and AutoUnmount are macOS-only: on Linux, fusermount3
        // requires `user_allow_other` in /etc/fuse.conf for allow_other — and
        // fuser's AutoUnmount implicitly INJECTS allow_other. The ordered
        // shutdown already unmounts; a post-crash zombie is recovered at boot.
        options.push(MountOption::AutoUnmount);
        options.push(MountOption::AllowOther);
        options.push(MountOption::CUSTOM("local".to_string()));
        options.push(MountOption::CUSTOM("volname=drive-sync".to_string()));
        options.push(MountOption::CUSTOM("noappledouble".to_string()));
        // macFUSE's default daemon_timeout is 60s. When pasting a folder with
        // many subfolders/files, the CREATE+WRITE+FLUSH+RELEASE cycle can
        // exceed that under I/O pressure. 300s gives headroom.
        options.push(MountOption::CUSTOM("daemon_timeout=300".to_string()));
    }

    // Post-crash recovery: kill -9 (or a panic) does not run AutoUnmount and
    // leaves a zombie FUSE mount that blocks remounting — launchd's KeepAlive
    // would keep restarting a daemon that fails forever. Detect and force the
    // unmount before mounting.
    prepare_mountpoint(&mount_path)?;

    // Background session: main owns the lifecycle and drives the ordered
    // shutdown (decisions 6 and 7 of Phase 2).
    let session = fuser::spawn_mount2(fs, &mount_path, &options)?;
    println!("[{}] boot: volume mounted at {}", log_ts(), mount_path);
    sync_handle.enable_notifications();
    sync_handle
        .notifier()
        .notify(drive_sync::notify::NotifyEvent::Mounted {
            mount: mount_path.clone(),
        });

    // Wait for SIGTERM/SIGINT (launchd/Ctrl-C) or for the session to end
    // externally (eject in Finder / umount).
    let ejected = runtime.block_on(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        loop {
            tokio::select! {
                _ = term.recv() => {
                    println!("[{}] shutdown: SIGTERM received", log_ts());
                    return Ok::<bool, anyhow::Error>(false);
                }
                _ = int.recv() => {
                    println!("[{}] shutdown: SIGINT received", log_ts());
                    return Ok(false);
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    if session.guard.is_finished() {
                        println!("[{}] shutdown: volume ejected externally", log_ts());
                        return Ok(true);
                    }
                }
            }
        }
    })?;

    // Shutdown order (decision 7): unmount BEFORE the final drain — the
    // unmount forces the kernel to deliver FLUSH/RELEASE for open fds, and
    // those ops enter the queue in time to be drained.
    if !ejected {
        println!("[{}] shutdown: unmounting {}", log_ts(), mount_path);
    }
    session.join();
    sync_handle.drain_now();
    sync_handle.status_state().record_stopped();
    sync_handle
        .notifier()
        .notify(drive_sync::notify::NotifyEvent::Stopped);
    println!("[{}] shutdown: clean stop completed", log_ts());

    Ok(())
}

/// Ensures the mountpoint exists and is free. If the mount table already
/// lists the path: probe with `read_dir` — a zombie mount (previous crash)
/// fails and gets `umount -f`; a healthy mount means another live instance,
/// and the boot aborts with a clear error instead of fighting over the
/// mountpoint.
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
            "a healthy volume is already mounted at {} — stop the other daemon or unmount it first",
            canonical
        ),
        Err(e) => {
            println!(
                "[{}] boot: zombie mount detected at {} ({}) — forcing unmount",
                log_ts(),
                canonical,
                e
            );
            // Linux: user FUSE mounts are unmounted via fusermount3 -u;
            // plain `umount` usually requires root.
            #[cfg(not(target_os = "macos"))]
            {
                let _ = std::process::Command::new("fusermount3")
                    .arg("-u")
                    .arg(&canonical)
                    .status();
            }
            let status = std::process::Command::new("umount")
                .arg("-f")
                .arg(&canonical)
                .status()?;
            if !status.success() {
                anyhow::bail!(
                    "could not unmount the zombie mount at {} — run `umount -f {}` manually",
                    canonical,
                    canonical
                );
            }
            // Give the kernel a moment to release the mountpoint.
            std::thread::sleep(std::time::Duration::from_millis(300));
            Ok(())
        }
    }
}

/// `drive-sync status`: reads the status.json written by the daemon and formats it.
fn run_status(file: &Path) -> anyhow::Result<()> {
    use drive_sync::status;
    match status::read_from(file) {
        Ok(s) => {
            let alive = status::pid_alive(s.pid);
            print!("{}", status::format_status(&s, status::now_unix(), alive));
        }
        Err(e) => {
            println!("No status available ({}).", e);
            println!(
                "Mount a repository first: drive-sync -r <repo> -m <mount> — the daemon writes status.json to the log dir (default: logs/ of the directory it was launched from). Use --file to point to another path."
            );
        }
    }
    Ok(())
}
