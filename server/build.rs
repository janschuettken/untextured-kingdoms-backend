//! Version für die Lobby: `GIT_REV` aus der Umgebung (Docker-Build, siehe
//! `deploy/update.sh`), sonst aus Git gelesen, sonst nur die Paketversion.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GIT_REV");
    // Neu bauen, wenn sich der Commit ändert.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    let rev = std::env::var("GIT_REV")
        .ok()
        .filter(|r| !r.trim().is_empty())
        .or_else(|| {
            let out = Command::new("git").args(["log", "-1", "--format=%h (%cs)"]).output().ok()?;
            let text = String::from_utf8(out.stdout).ok()?;
            out.status.success().then(|| text.trim().to_string()).filter(|t| !t.is_empty())
        });
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match rev {
        Some(rev) => format!("{pkg} · {}", rev.trim()),
        None => pkg,
    };
    println!("cargo:rustc-env=UK_VERSION={version}");
}
