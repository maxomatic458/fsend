//! Generates shell completions and a man page into `dist/` for release packaging.

use clap::CommandFactory;
use clap_complete::{generate_to, Shell};
use std::{env, ffi::OsStr, fs, io, path::Path};

include!("src/cli.rs");

/// Cargo builds from a copy it owns during `cargo package`/`publish` and
/// `cargo install`.
fn is_managed_checkout(dir: &Path) -> bool {
    // `cargo install` unpacks to `<registry>/src/<index>/<pkg>-<ver>`
    let from_registry = dir
        .iter()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0] == "registry" && w[1] == "src");

    // Verify builds run from `<target-dir>/package/<pkg>-<ver>`. `<target-dir>`
    // is whatever `--target-dir` says, so match the parent name alone — keying
    // on a literal `target/package` misses `--target-dir target/publish`.
    let from_package = dir.parent().and_then(Path::file_name) == Some(OsStr::new("package"));

    from_registry || from_package
}

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset"));
    if is_managed_checkout(&manifest_dir) {
        return Ok(());
    }

    // Re-runs only when the files above change; `touch build.rs` to rebuild a
    // deleted dist/.
    let completions = manifest_dir.join("dist/completions");
    let man_dir = manifest_dir.join("dist/man");
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
        generate_to(shell, &mut cmd, "fsend", &completions)?;
    }

    let mut man = Vec::new();
    clap_mangen::Man::new(cmd).render(&mut man)?;
    fs::write(man_dir.join("fsend.1"), man)?;

    Ok(())
}
