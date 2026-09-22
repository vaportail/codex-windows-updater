//! Store-updater ABI adapter. No package deployment or process-wide API hooks.
use napi::{bindgen_prelude::AsyncTask, Env, Error, JsFunction, Result, Task};
use napi_derive::napi;
use serde::Deserialize;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const LAUNCHER_ENV: &str = "CODEX_UPDATER_LAUNCHER";

fn launcher() -> Result<PathBuf> {
    let path =
        PathBuf::from(std::env::var_os(LAUNCHER_ENV).ok_or_else(|| {
            Error::from_reason("Codex must be started through codex-launcher.exe")
        })?);
    if !path.is_absolute()
        || !path.is_file()
        || !path
            .parent()
            .is_some_and(|p| p.join("updater.json").is_file())
    {
        return Err(Error::from_reason(
            "Invalid Codex launcher path or missing updater.json",
        ));
    }
    Ok(path)
}

fn command() -> Result<Command> {
    let path = launcher()?;
    let mut cmd = Command::new(&path);
    cmd.current_dir(path.parent().unwrap())
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW (console helper only)
    }
    Ok(cmd)
}

#[derive(Deserialize)]
struct CheckReply {
    protocol: u32,
    available: bool,
    error: Option<String>,
}

#[napi(object)]
pub struct StoreResult {
    pub has_update: bool,
    pub can_silently_download: bool,
    pub completed: bool,
    pub overall_state: String,
}

impl StoreResult {
    fn completed(available: bool) -> Self {
        Self {
            has_update: available,
            can_silently_download: true,
            completed: true,
            overall_state: "Completed".into(),
        }
    }
}

pub struct CheckTask;
impl Task for CheckTask {
    type Output = bool;
    type JsValue = StoreResult;

    fn compute(&mut self) -> Result<bool> {
        let mut child = command()?
            .arg("--bridge-check")
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| Error::from_reason(format!("Starting update check: {e}")))?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < Duration::from_secs(120) => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                outcome => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::from_reason(format!(
                        "Update check timed out or failed: {outcome:?}"
                    )));
                }
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        if !output.status.success() {
            return Err(Error::from_reason("Launcher update check failed"));
        }
        let reply: CheckReply = serde_json::from_slice(&output.stdout)
            .map_err(|e| Error::from_reason(format!("Invalid launcher reply: {e}")))?;
        if reply.protocol != 1 {
            return Err(Error::from_reason("Unsupported launcher bridge protocol"));
        }
        if let Some(error) = reply.error {
            return Err(Error::from_reason(error));
        }
        Ok(reply.available)
    }

    fn resolve(&mut self, _env: Env, output: bool) -> Result<StoreResult> {
        Ok(StoreResult::completed(output))
    }
}

#[napi]
pub fn try_silent_download_store_updates(_progress: JsFunction) -> AsyncTask<CheckTask> {
    // Availability enables Codex's install button. Download and verification
    // happen in our updater after the user requests installation.
    AsyncTask::new(CheckTask)
}

#[napi]
pub fn try_silent_download_and_install_store_updates(_progress: JsFunction) -> Result<StoreResult> {
    // Let the caller's normal completion path signal its shutdown. Do not
    // invoke the Deploying callback before the new updater process exists.
    command()?
        .arg("--bridge-install")
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| Error::from_reason(format!("Starting Codex updater: {e}")))?;
    Ok(StoreResult::completed(true))
}

#[napi]
pub fn get_current_package_family() -> String {
    // JS uses this for its update state namespace and sandbox configuration.
    // It is the production manifest's family, NOT a Windows registration.
    "OpenAI.Codex_2p2nqsd0c76g0".into()
}

#[napi]
pub fn arm_process_tree_cleanup() -> Result<()> {
    // Disable the MSIX fallback: our updater owns staging and process cleanup.
    Err(Error::from_reason(
        "MSIX deployment is managed by codex-launcher",
    ))
}

#[napi]
pub fn get_package_local_cache_path() -> Option<String> {
    None
}

#[napi]
pub fn read_package_metadata(_path: String) -> Result<()> {
    Err(Error::from_reason(
        "MSIX deployment is managed by codex-launcher",
    ))
}

#[napi]
pub fn stage_package(_path: String) -> Result<()> {
    Err(Error::from_reason(
        "MSIX deployment is managed by codex-launcher",
    ))
}

#[napi]
pub fn activate_staged_package(_path: String) -> Result<()> {
    Err(Error::from_reason(
        "MSIX deployment is managed by codex-launcher",
    ))
}
