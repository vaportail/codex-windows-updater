use anyhow::{Context, Result};
use std::path::PathBuf;
fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let exe = PathBuf::from(
        args.next()
            .context("usage: codex-identity-launcher.exe EXE MANIFEST [app arguments...]")?,
    );
    let manifest = PathBuf::from(args.next().context("missing manifest path")?);
    let dll = std::env::current_exe()?.with_file_name("codex_identity_shim.dll");
    let pid = codex_identity_launcher::launch(&exe, &dll, &manifest, &args.collect::<Vec<_>>())?;
    println!("Identity hooks installed before entry; started PID {pid}");
    if std::env::var_os("CODEX_IDENTITY_TRACE").is_some() {
        use windows_sys::Win32::{Foundation::*, System::Threading::*};
        unsafe {
            let process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if !process.is_null() {
                if WaitForSingleObject(process, 10000) == WAIT_OBJECT_0 {
                    let mut code = 0;
                    GetExitCodeProcess(process, &mut code);
                    println!("Child exited with code {code:#x}");
                } else {
                    println!("Child is still running");
                }
                CloseHandle(process);
            }
        }
    }
    Ok(())
}
