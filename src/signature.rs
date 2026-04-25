//! Authenticode signature verification for downloaded MSIX packages.
//!
//! Closes the trust chain we'd otherwise be relying on HTTPS + the Store CDN
//! for. The MSIX format embeds an Authenticode signature in
//! `AppxSignature.p7x`; Windows wires this through the SIP infrastructure
//! so a vanilla `WinVerifyTrust(WTD_CHOICE_FILE)` call validates the
//! package signature and certificate chain just like it would for a PE.
//!
//! Returns `Ok(())` when the file is signed by a chain rooted in a trusted
//! store and the signature itself is intact. Any verification failure —
//! unsigned, tampered, expired chain, untrusted root — surfaces as an
//! `Err` so the caller can refuse to extract.

use anyhow::Result;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
    WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE,
};

/// `WINTRUST_ACTION_GENERIC_VERIFY_V2` — the standard Authenticode policy
/// provider GUID. Tells `WinVerifyTrust` to do a full signature + chain
/// validation, picking the appropriate Subject Interface Package (SIP)
/// based on file type. For .msix this routes through the AppX SIP.
const WINTRUST_ACTION_GENERIC_VERIFY_V2: GUID =
    GUID::from_u128(0x00aac56b_cd44_11d0_8cc2_00c04fc295ee);

/// Verify the Authenticode signature on `path`. Returns `Ok(())` on valid
/// signature + trusted chain, `Err` otherwise. The returned error includes
/// the `HRESULT` from `WinVerifyTrust` for diagnostics.
pub fn verify_msix(path: &Path) -> Result<()> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        ..Default::default()
    };

    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &file_info as *const _ as *mut _,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        ..Default::default()
    };

    let mut policy = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    let status = unsafe { WinVerifyTrust(HWND::default(), &mut policy, &mut data as *mut _ as _) };

    // Always pair STATEACTION_VERIFY with STATEACTION_CLOSE so WinTrust
    // releases its internal state.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = unsafe { WinVerifyTrust(HWND::default(), &mut policy, &mut data as *mut _ as _) };

    if status == 0 {
        Ok(())
    } else {
        anyhow::bail!(
            "Authenticode verification failed for {}: HRESULT 0x{:08X}",
            path.display(),
            status as u32
        )
    }
}
