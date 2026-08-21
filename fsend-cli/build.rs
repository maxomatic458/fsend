//! Generates shell completions and a man page at build time.

use std::{env, fs, io};
use clap::CommandFactory;
use clap_complete::{generate, Generator, Shell};

include!("src/cli.rs");

/// Writes `contents` only when it differs from what is already on disk.
///
/// The generated files are watched via `rerun-if-changed`, so rewriting them
/// unconditionally would bump their mtimes on every build, invalidate this
/// script, and make every `cargo build` re-run it and relink.
fn write_if_changed(path: &std::path::Path, contents: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    fs::write(path, contents)
}

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=build.rs");
    // The generated files live outside OUT_DIR, so cargo does not know they are
    // this script's output. Without this, deleting dist/ leaves the build script
    // "fresh" and the completions and man page are silently never regenerated --
    // which would ship release archives missing them if the build is ever cached.
    println!("cargo:rerun-if-changed=dist");

    let dist = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset"))
        .join("dist");
    let completions = dist.join("completions");
    let man_dir = dist.join("man");
    fs::create_dir_all(&completions)?;
    fs::create_dir_all(&man_dir)?;

    let mut cmd = Args::command();

    for shell in [
        Shell::Bash,
        Shell::Zsh,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Elvish,
    ] {
        let mut buf = Vec::new();
        generate(shell, &mut cmd, "fsend", &mut buf);
        write_if_changed(&completions.join(shell.file_name("fsend")), &buf)?;
    }

    let mut man = Vec::new();
    clap_mangen::Man::new(cmd).render(&mut man)?;
    write_if_changed(&man_dir.join("fsend.1"), &man)?;

    Ok(())
}
