//! Geração e gestão do LaunchAgent do macOS.
//!
//! O plist é gerado por template de string (decisão 4 da Fase 2): estrutura
//! pequena e fixa, auditável no diff e nos testes. Os paths de config, log e
//! status vão como flags explícitas no ProgramArguments — o plist é a fonte
//! da verdade legível de onde o daemon lê e grava (decisão 10).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::log_ts;
use crate::paths;

pub const LABEL: &str = "dev.giacomeli.drive-sync";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Path do plist do agente: `~/Library/LaunchAgents/<label>.plist`.
pub fn agent_plist_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", LABEL))
}

/// Renderiza o plist do LaunchAgent. Puro e testável.
///
/// - `RunAtLoad`: monta no login.
/// - `KeepAlive {SuccessfulExit=false}`: crash religa; parada limpa
///   (SIGTERM/eject, exit 0) NÃO ressuscita (decisão 5).
/// - `ExitTimeOut 60`: margem para o drain final antes do SIGKILL do launchd.
pub fn render_plist(
    binary: &Path,
    repo: &Path,
    mount: &Path,
    config: &Path,
    log_dir: &Path,
    status_file: &Path,
) -> String {
    let e = |p: &Path| xml_escape(&p.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>-r</string>
        <string>{repo}</string>
        <string>-m</string>
        <string>{mount}</string>
        <string>--config</string>
        <string>{config}</string>
        <string>--log-dir</string>
        <string>{log_dir}</string>
        <string>--status-file</string>
        <string>{status}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ExitTimeOut</key>
    <integer>60</integer>
    <key>StandardOutPath</key>
    <string>{log_dir}/launchd-stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/launchd-stderr.log</string>
</dict>
</plist>
"#,
        label = LABEL,
        bin = e(binary),
        repo = e(repo),
        mount = e(mount),
        config = e(config),
        log_dir = e(log_dir),
        status = e(status_file),
    )
}

fn launchctl(args: &[&str]) -> anyhow::Result<std::process::Output> {
    Ok(Command::new("launchctl").args(args).output()?)
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Instala o LaunchAgent: resolve paths absolutos, prepara diretórios padrão,
/// copia o config se necessário, grava o plist e registra no launchd.
pub fn install(repo: &str, mount: &str) -> anyhow::Result<()> {
    let binary = std::env::current_exe()?;
    let repo = std::fs::canonicalize(repo)
        .map_err(|e| anyhow::anyhow!("repo {} inacessível: {}", repo, e))?;
    std::fs::create_dir_all(mount)?;
    let mount = std::fs::canonicalize(mount)?;

    let app_support = paths::app_support_dir();
    let log_dir = paths::standard_log_dir();
    std::fs::create_dir_all(&app_support)?;
    std::fs::create_dir_all(&log_dir)?;

    // Config: se o padrão não existe e há um ./config.json no CWD (fluxo dev),
    // copia — senão o daemon parte dos defaults (sem identidade = sem commit).
    let config = paths::standard_config_path();
    if !config.exists() {
        if Path::new("config.json").exists() {
            std::fs::copy("config.json", &config)?;
            println!(
                "[{}] install: config.json copiado para {}",
                log_ts(),
                config.display()
            );
        } else {
            println!(
                "[{}] install: sem config.json — crie {} com user_name/user_email para habilitar commits",
                log_ts(),
                config.display()
            );
        }
    }

    let status_file = paths::standard_status_path();
    let plist = render_plist(&binary, &repo, &mount, &config, &log_dir, &status_file);
    let plist_path = agent_plist_path();
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist_path, plist)?;

    // Re-install idempotente: derruba a instância anterior (se houver) antes
    // de registrar de novo. bootout de agente inexistente falha — ignorado.
    let _ = launchctl(&["bootout", &format!("{}/{}", gui_domain(), LABEL)]);
    let out = launchctl(&["bootstrap", &gui_domain(), plist_path.to_str().unwrap()])?;
    if !out.status.success() {
        anyhow::bail!(
            "launchctl bootstrap falhou: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    println!("[{}] install: LaunchAgent registrado ({})", log_ts(), LABEL);
    println!("  plist:  {}", plist_path.display());
    println!("  repo:   {}", repo.display());
    println!("  mount:  {}", mount.display());
    println!("  config: {}", config.display());
    println!("  logs:   {}", log_dir.display());
    println!("  status: {} (drive-sync status acha sozinho)", status_file.display());
    Ok(())
}

/// Remove o LaunchAgent: bootout + remoção do plist.
pub fn uninstall() -> anyhow::Result<()> {
    let out = launchctl(&["bootout", &format!("{}/{}", gui_domain(), LABEL)])?;
    if out.status.success() {
        println!("[{}] uninstall: agente parado e desregistrado", log_ts());
    } else {
        println!(
            "[{}] uninstall: agente não estava registrado ({})",
            log_ts(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let plist_path = agent_plist_path();
    match std::fs::remove_file(&plist_path) {
        Ok(()) => println!("[{}] uninstall: {} removido", log_ts(), plist_path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plist_contains_program_arguments_in_order() {
        let plist = render_plist(
            Path::new("/usr/local/bin/drive-sync"),
            Path::new("/Users/x/repo"),
            Path::new("/Users/x/GitDrive"),
            Path::new("/Users/x/Library/Application Support/drive-sync/config.json"),
            Path::new("/Users/x/Library/Logs/drive-sync"),
            Path::new("/Users/x/Library/Application Support/drive-sync/status.json"),
        );
        assert!(plist.contains("<string>dev.giacomeli.drive-sync</string>"));
        let bin = plist.find("<string>/usr/local/bin/drive-sync</string>").unwrap();
        let repo = plist.find("<string>/Users/x/repo</string>").unwrap();
        let mount = plist.find("<string>/Users/x/GitDrive</string>").unwrap();
        assert!(bin < repo && repo < mount, "ordem dos argumentos deve ser preservada");
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(plist.contains("<key>ExitTimeOut</key>"));
    }

    #[test]
    fn render_plist_escapes_xml_in_paths() {
        let plist = render_plist(
            Path::new("/bin/drive-sync"),
            Path::new("/Users/x/repo & <coisas>"),
            Path::new("/mnt"),
            Path::new("/cfg.json"),
            Path::new("/logs"),
            Path::new("/status.json"),
        );
        assert!(plist.contains("/Users/x/repo &amp; &lt;coisas&gt;"));
        assert!(!plist.contains("repo & <coisas>"));
    }
}
