mod fs;
mod git;

use fs::GitDriveFS;
use clap::{Arg, Command};

fn main() -> anyhow::Result<()> {
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
        .get_matches();

    let repo_path = matches.get_one::<String>("repo").unwrap();
    let mount_path = matches.get_one::<String>("mount").unwrap();

    println!("🔗 Repositório Git: {}", repo_path);
    println!("📂 Montando em: {}", mount_path);

    let fs = GitDriveFS::new(repo_path)?;
    fuser::mount2(fs, mount_path, &[])?;

    Ok(())
}