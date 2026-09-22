//! Configurable sidecar bridge, embedded by the standard build script.
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn launcher_for(root: &Path) -> Result<std::path::PathBuf> {
    let current = std::env::current_exe()?;
    let root = root.canonicalize()?;
    if current
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .as_ref()
        == Some(&root)
    {
        return Ok(current);
    }
    // Fresh-install Launch can run from a downloaded installer. Hand off to
    // its installed copy, whose adjacent updater.json describes this root.
    let installed = root.join("codex-launcher.exe");
    if !installed.is_file() {
        bail!("Installed launcher is missing: {}", installed.display());
    }
    Ok(installed)
}

#[cfg(native_updater_bridge)]
const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/windows-updater.node"));
#[cfg(not(native_updater_bridge))]
const PAYLOAD: &[u8] = &[];

pub fn prepare(app_dir: &Path, enabled: bool) -> Result<()> {
    if enabled && PAYLOAD.is_empty() {
        bail!("This development build has no updater sidecar. Build with ./build.ps1, or disable native_updater_bridge in updater.json.");
    }
    prepare_payload(app_dir, if enabled { PAYLOAD } else { &[] })
}

fn prepare_payload(app_dir: &Path, payload: &[u8]) -> Result<()> {
    let source = app_dir.join("resources/native/windows-updater.node");
    let marker = source.with_file_name("windows-updater.launcher.sha256");
    if source.is_file() {
        let bytes = std::fs::read(&source)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let owned = std::fs::read_to_string(&marker).is_ok_and(|s| s.trim() == digest);
        if owned {
            if bytes == payload {
                return Ok(());
            }
            std::fs::remove_file(&source)
                .context("replacing launcher updater sidecar (close Codex first)")?;
        } else {
            crate::extract::disable_native_updater(app_dir)?;
        }
    }
    if payload.is_empty() {
        return Ok(());
    }
    // Only replace the known sidecar layout. Older packages may have no addon.
    if !source.with_file_name("windows-updater.broken").is_file() {
        return Ok(());
    }
    let staging = source.with_file_name(format!("windows-updater.{}.partial", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        use std::io::Write;
        file.write_all(payload)?;
        file.sync_all()?;
        drop(file);
        // Publish complete bytes; a failed write cannot become a loadable addon.
        std::fs::rename(&staging, &source).context("publishing native updater bridge")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result?;
    std::fs::write(marker, format!("{:x}", Sha256::digest(payload)))?;
    Ok(())
}

pub fn check() -> Result<()> {
    // Read-only: no UI, config writes, cleanup, or process termination.
    let root = crate::mode::install_root()?;
    let cfg = crate::config::Config::load_runtime(&root)?;
    let decision = if cfg.native_updater_bridge {
        crate::updater::check_now(&cfg, crate::store::PRODUCT_ID_CODEX)
    } else {
        crate::updater::UpdateDecision::UpToDate {
            version: cfg.current_version.clone(),
        }
    };
    let (available, error) = match decision {
        crate::updater::UpdateDecision::Available { .. } => (true, None),
        crate::updater::UpdateDecision::UpToDate { .. } => (false, None),
        crate::updater::UpdateDecision::Error(e) => {
            // Bound even JSON-escaped errors below the anonymous pipe buffer.
            (false, Some(e.chars().take(512).collect::<String>()))
        }
        _ => (false, Some("Unexpected update check result".into())),
    };
    serde_json::to_writer(
        std::io::stdout().lock(),
        &serde_json::json!({
            "protocol": 1, "available": available, "error": error
        }),
    )?;
    Ok(())
}

pub fn install() -> Result<()> {
    let root = crate::mode::install_root()?;
    let cfg = crate::config::Config::load_runtime(&root)?;
    if !cfg.native_updater_bridge {
        bail!("In-app updates are disabled in updater.json");
    }
    if matches!(cfg.install_mode, crate::config::InstallMode::System)
        && !crate::elevate::is_elevated()
    {
        return crate::elevate::respawn_elevated("--bridge-install");
    }
    close_for_install(&root)?;
    crate::run_proxy(cfg, None, &[], true)
}

fn close_for_install(root: &Path) -> Result<()> {
    // The in-app Install action already authorizes closing this installation.
    // Give Codex's normal update shutdown callback a chance to finish first.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let versions = root.join("versions");
    let pids = crate::proxy::find_our_codex_pids(&versions);
    crate::proxy::terminate_pids(&pids, 5000);
    if !crate::proxy::find_our_codex_pids(&versions).is_empty() {
        bail!("Could not close Codex for the requested update");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_original_and_only_replaces_owned_bridge() {
        let root = std::env::temp_dir().join(format!("codex-bridge-test-{}", std::process::id()));
        let native = root.join("resources/native");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("windows-updater.node"), b"original").unwrap();
        prepare_payload(&root, b"shim-v1").unwrap();
        prepare_payload(&root, b"shim-v1").unwrap();
        prepare_payload(&root, b"shim-v2").unwrap();
        assert_eq!(
            std::fs::read(native.join("windows-updater.broken")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read(native.join("windows-updater.node")).unwrap(),
            b"shim-v2"
        );
        prepare_payload(&root, b"").unwrap();
        assert!(!native.join("windows-updater.node").exists());
        prepare_payload(&root, b"shim-v2").unwrap();
        std::fs::write(native.join("windows-updater.node"), b"unknown").unwrap();
        assert!(prepare_payload(&root, b"shim-v3").is_err());
        assert_eq!(
            std::fs::read(native.join("windows-updater.node")).unwrap(),
            b"unknown"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
