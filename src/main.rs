use clap::{Arg, Command};
use drive_sync::config::Config;
use drive_sync::fs::GitDriveFS;
use drive_sync::git;
use drive_sync::log_ts;
use drive_sync::sync::SyncManager;
use fuser::MountOption;
use std::io::Write;

/// Instala um panic hook que loga o backtrace completo antes do unwind chegar
/// no boundary do `fuser::mount2` e matar o processo. Sem isso, panics em
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let matches = Command::new("drive-sync")
        .version("0.1.0")
        .about("Sistema de arquivos versionado com Git")
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
                .help("Espelha os logs no terminal além de gravar em logs/")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();

    let repo_path = matches.get_one::<String>("repo").unwrap().clone();
    let mount_path = matches.get_one::<String>("mount").unwrap().clone();
    let verbose = matches.get_flag("verbose");

    // Depois do parse dos args (pra `--help`/`--version` ainda saírem no
    // terminal): redirecionar stdout/stderr pra `logs/drive-sync-<ts>.log`.
    // Isso captura nossos println!/eprintln! E o output do crate `log`
    // (fuser via env_logger) num único arquivo. Anuncia o path diretamente
    // em `/dev/tty` pra que o usuário saiba onde olhar.
    let log_path = drive_sync::redirect_io_to_log_file(verbose).ok();
    if let Some(ref path) = log_path {
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = writeln!(tty, "log em: {}", path.display());
        }
    }

    // Hook precisa rodar DEPOIS do redirect_io_to_log_file pra que o stderr do
    // hook caia no arquivo de log junto do resto do output.
    install_panic_hook();

    // env_logger respeita RUST_LOG. Com RUST_LOG=fuser=debug, a gente vê o
    // opcode cru que macFUSE envia, útil pra diagnosticar discrepâncias entre
    // o que o kernel manda e o que a gente interpreta.
    env_logger::init();

    let config = Config::load_from("config.json");
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
    let fs = GitDriveFS::new(&repo_path, branch, sync)?;

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
        // esse tempo se houver pressão de I/O ou se o debounce/commit/push
        // estiver bloqueando. O kernel mata o mount quando o timeout estoura,
        // o que vira "(error code -8062)" no Finder. 300s dá margem.
        options.push(MountOption::CUSTOM("daemon_timeout=300".to_string()));
    }

    fuser::mount2(fs, &mount_path, &options)?;

    Ok(())
}