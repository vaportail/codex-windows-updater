//! End-to-end host: Chromium-like host stays unpackaged; native addon sees identity.
use anyhow::{ensure, Result};
use windows_sys::Win32::Storage::Packaging::Appx::*;
fn main() -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let mut len = 0;
        ensure!(
            GetCurrentPackageId(&mut len, std::ptr::null_mut()) == 15700,
            "host must remain unpackaged"
        );
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        let addon: Vec<u16> = args[0].encode_wide().chain([0]).collect();
        let module = LoadLibraryW(addon.as_ptr());
        ensure!(!module.is_null(), "probe addon failed to load");
        let function =
            GetProcAddress(module, c"VerifyIdentity".as_ptr() as _).expect("probe export");
        let function: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32 =
            std::mem::transmute(function);
        ensure!(
            function(std::ptr::null_mut()) == 0,
            "native addon identity checks failed"
        );
        len = 0;
        ensure!(
            GetCurrentPackageId(&mut len, std::ptr::null_mut()) == 15700,
            "host identity changed after addon load"
        );
        let forwarded = args[2..]
            .iter()
            .map(|a| a.to_str().unwrap())
            .collect::<Vec<_>>();
        std::fs::write(&args[1], format!("{forwarded:?}"))?;
    }
    Ok(())
}
