//! Proxy-mode runtime: resolve the newest installed Codex.exe (self-healing
//! the `versions/current` junction if needed) and spawn it with the caller's
//! args + inherited env. If Codex is already running, no-op — Codex ships
//! its own single-instance mutex, so launching again would be a confusing
//! no-op anyway.

use crate::config::Config;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Same self-heal as the installer Launch button — delegates to
/// `main::latest_codex_exe` so the logic lives in one place.
pub fn resolve_codex_exe(root: &Path, use_junction: bool) -> Option<PathBuf> {
    crate::latest_codex_exe(root, use_junction)
}

/// Spawn Codex.exe with forwarded args. Env is inherited by default.
/// Returns `Ok(())` even when Codex is already running — the launcher's
/// job is done in that case.
pub fn launch(root: &Path, cfg: &Config, forward_args: &[String]) -> Result<()> {
    let exe = resolve_codex_exe(root, cfg.use_current_junction)
        .ok_or_else(|| anyhow::anyhow!("no installed Codex.exe found under {}", root.display()))?;

    if is_codex_running() {
        eprintln!("Codex already running; not spawning another instance");
        return Ok(());
    }

    // Working dir = the versioned install dir so relative resource lookups
    // (Electron's default) resolve against the app root.
    let working_dir = exe.parent().unwrap_or(root);
    std::process::Command::new(&exe)
        .args(forward_args)
        .current_dir(working_dir)
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    Ok(())
}

/// Cheap boolean variant — is anything named Codex.exe running?
pub fn is_codex_running() -> bool {
    !find_codex_pids().is_empty()
}

/// Walk the process table collecting PIDs of every process named `Codex.exe`.
/// Electron apps fork multiple processes (main + renderer + GPU + utility),
/// all typically sharing the same exe name — callers that intend to terminate
/// Codex should kill every PID returned here, not just the first.
///
/// We skip our own PID so a hypothetical rename of the launcher to Codex.exe
/// wouldn't self-match.
#[cfg(windows)]
pub fn find_codex_pids() -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let target = "codex.exe";
    let current_pid = std::process::id();
    let mut pids = Vec::new();

    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return pids,
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID != current_pid {
                    let end = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name =
                        String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase();
                    if name == target {
                        pids.push(entry.th32ProcessID);
                    }
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    pids
}

#[cfg(not(windows))]
pub fn find_codex_pids() -> Vec<u32> {
    Vec::new()
}

/// Terminate every PID in `pids` and wait up to `wait_ms` total for each to
/// exit so file locks release before we try to delete the exes. Silently
/// skips PIDs that we can't open (already dead, access denied).
#[cfg(windows)]
pub fn terminate_pids(pids: &[u32], wait_ms: u32) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    for &pid in pids {
        unsafe {
            let handle = match OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) {
                Ok(h) => h,
                Err(_) => continue,
            };
            let _ = TerminateProcess(handle, 1);
            let wait_result = WaitForSingleObject(handle, wait_ms);
            if wait_result != WAIT_OBJECT_0 {
                eprintln!("warn: pid {pid} didn't exit within {wait_ms}ms");
            }
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(not(windows))]
pub fn terminate_pids(_pids: &[u32], _wait_ms: u32) {}
