//! Test fixture loaded under the native addon's basename; never distributed.
use anyhow::{ensure, Result};
use windows::{
    ApplicationModel::Package,
    System::ProcessorArchitecture,
    Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
};
use windows_sys::Win32::{Storage::Packaging::Appx::*, System::Threading::GetCurrentProcess};

#[no_mangle]
pub extern "system" fn VerifyIdentity(_: *mut std::ffi::c_void) -> u32 {
    match verify() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("identity probe: {error:#}");
            1
        }
    }
}
fn verify() -> Result<()> {
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED)?;
        let package = Package::Current()?;
        let winrt_id = package.Id()?;
        ensure!(
            winrt_id.FamilyName()? == "OpenAI.Codex_2p2nqsd0c76g0",
            "WinRT family differs"
        );
        ensure!(winrt_id.Name()? == "OpenAI.Codex", "WinRT name differs");
        ensure!(
            winrt_id.Publisher()? == "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
            "publisher differs"
        );
        ensure!(
            winrt_id.Architecture()? == ProcessorArchitecture::X64,
            "architecture differs"
        );
        let version = winrt_id.Version()?;
        ensure!(
            (
                version.Major,
                version.Minor,
                version.Build,
                version.Revision
            ) == (26, 915, 4065, 0),
            "version differs"
        );
        let mut len = 0;
        ensure!(
            GetCurrentPackageId(&mut len, std::ptr::null_mut()) == 122,
            "ID size query failed"
        );
        let mut buffer = vec![0u8; len as usize];
        ensure!(
            GetCurrentPackageId(&mut len, buffer.as_mut_ptr()) == 0,
            "ID read failed"
        );
        let id = std::ptr::read_unaligned(buffer.as_ptr() as *const PACKAGE_ID);
        let mut full_len = 0;
        ensure!(
            GetCurrentPackageFullName(&mut full_len, std::ptr::null_mut()) == 122,
            "full name query failed"
        );
        let mut full = vec![0u16; full_len as usize];
        ensure!(
            GetCurrentPackageFullName(&mut full_len, full.as_mut_ptr()) == 0,
            "full name read failed"
        );
        let mut expected = vec![0u16; full_len as usize];
        ensure!(
            PackageFullNameFromId(&id, &mut full_len, expected.as_mut_ptr()) == 0
                && full == expected,
            "Win32 ID/name mismatch"
        );
        ensure!(
            winrt_id.FullName()? == String::from_utf16_lossy(&full[..full.len() - 1]),
            "WinRT/Win32 names differ"
        );
        let mut family_len = 0;
        ensure!(
            GetPackageFamilyName(GetCurrentProcess(), &mut family_len, std::ptr::null_mut()) == 122,
            "process-handle family query failed"
        );
        let mut family = vec![0; family_len as usize];
        ensure!(
            GetCurrentPackageFamilyName(&mut family_len, family.as_mut_ptr()) == 0,
            "current family query failed"
        );
        ensure!(
            winrt_id.FamilyName()? == String::from_utf16_lossy(&family[..family.len() - 1]),
            "family mismatch"
        );
    }
    Ok(())
}
