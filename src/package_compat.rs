//! Supplies manifest identity to unpackaged installs before the app entry point.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const SHIM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/codex_identity_shim.dll"));

pub fn launch(exe: &Path, manifest: &Path, args: &[String]) -> Result<()> {
    let shim = materialize_shim()?;
    let args = args
        .iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    let pid = codex_identity_launcher::launch(exe, &shim, manifest, &args)
        .context("launching with package identity compatibility shim")?;
    crate::log_event(&format!(
        "package identity shim initialized before entry for PID {pid}"
    ));
    Ok(())
}

fn materialize_shim() -> Result<PathBuf> {
    if SHIM.is_empty() {
        let sibling = std::env::current_exe()?.with_file_name("codex_identity_shim.dll");
        ensure!(sibling.is_file(), "identity shim missing: build with ./build.ps1, or build codex-identity-shim beside the launcher");
        return Ok(sibling);
    }
    // Per-user, content-addressed cache also works for read-only System installs.
    let digest = format!("{:x}", Sha256::digest(SHIM));
    let dir = directories::BaseDirs::new()
        .context("local app data unavailable")?
        .data_local_dir()
        .join("codex-launcher")
        .join("identity-shims")
        .join(&digest);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("codex_identity_shim.dll");
    if let Ok(bytes) = std::fs::read(&path) {
        ensure!(
            Sha256::digest(&bytes) == Sha256::digest(SHIM),
            "cached identity shim hash mismatch: {}",
            path.display()
        );
        return Ok(path);
    }
    let temp = dir.join(format!("shim-{}.tmp", std::process::id()));
    std::fs::write(&temp, SHIM)?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        // Another concurrent launcher may have installed the same content.
        ensure!(
            std::fs::read(&path)
                .map(|b| Sha256::digest(&b) == Sha256::digest(SHIM))
                .unwrap_or(false),
            "installing shim cache: {error}"
        );
    }
    Ok(path)
}
