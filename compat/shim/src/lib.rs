//! Initialization is explicit, outside DllMain, while the app entry thread is held.
mod winrt;
use codex_package_identity::{write_string, Identity};
use retour::GenericDetour;
use std::{ffi::c_void, os::windows::ffi::OsStringExt, sync::OnceLock};
use windows_sys::Win32::{
    Foundation::*,
    Storage::Packaging::Appx::*,
    System::{LibraryLoader::*, Memory::*, Threading::*},
};

type LoadFn = unsafe extern "system" fn(*const u16, HANDLE, u32) -> HMODULE;
type LoadSimpleFn = unsafe extern "system" fn(*const u16) -> HMODULE;
static IDENTITY: OnceLock<Identity> = OnceLock::new();
static TRACE: OnceLock<std::fs::File> = OnceLock::new();
fn trace(message: &[u8]) {
    use std::io::Write;
    if let Some(mut file) = TRACE.get() {
        let _ = file.write_all(message);
    }
}
static ROOT: OnceLock<Vec<u16>> = OnceLock::new();
static LOADER: OnceLock<GenericDetour<LoadFn>> = OnceLock::new();
static SIMPLE_LOADER: OnceLock<GenericDetour<LoadSimpleFn>> = OnceLock::new();

unsafe extern "system" fn package_path(len: *mut u32, out: *mut u16) -> i32 {
    trace(b"GetCurrentPackagePath\n");
    write_string(ROOT.get().unwrap(), len, out)
}
unsafe extern "system" fn family_for_process(process: HANDLE, len: *mut u32, out: *mut u16) -> u32 {
    if GetProcessId(process) == GetCurrentProcessId() {
        family_name(len, out) as u32
    } else {
        GetPackageFamilyName(process, len, out)
    }
}
unsafe extern "system" fn full_for_process(process: HANDLE, len: *mut u32, out: *mut u16) -> u32 {
    if GetProcessId(process) == GetCurrentProcessId() {
        full_name(len, out) as u32
    } else {
        GetPackageFullName(process, len, out)
    }
}
unsafe extern "system" fn load_library(path: *const u16, file: HANDLE, flags: u32) -> HMODULE {
    let module = LOADER.get().unwrap().call(path, file, flags);
    let error = GetLastError();
    if !module.is_null() && (module as usize & 3) == 0 {
        patch_if_addon(module);
    }
    SetLastError(error);
    module
}
unsafe extern "system" fn load_simple(path: *const u16) -> HMODULE {
    let module = SIMPLE_LOADER.get().unwrap().call(path);
    let error = GetLastError();
    if !module.is_null() && (module as usize & 3) == 0 {
        patch_if_addon(module);
    }
    SetLastError(error);
    module
}
unsafe fn patch_if_addon(module: HMODULE) {
    let mut path = [0u16; 32768];
    let len = GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) as usize;
    if len == 0 || len >= path.len() {
        return;
    }
    let name = String::from_utf16_lossy(&path[..len]);
    let basename = name.rsplit(['\\', '/']).next().unwrap_or("");
    if !basename.eq_ignore_ascii_case("windows-updater.node")
        && !basename.eq_ignore_ascii_case("windows-account.node")
    {
        return;
    }
    match patch_imports(module) {
        Some(count) if count > 0 => trace(b"Patched native addon package imports\n"),
        _ => trace(b"ERROR: native addon has no patchable package imports\n"),
    }
}

unsafe fn patch_imports(module: HMODULE) -> Option<usize> {
    static PATCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = PATCH_LOCK.lock().ok()?;
    let base = module as *mut u8;
    let pe = std::ptr::read_unaligned(base.add(0x3c) as *const u32) as usize;
    if std::ptr::read_unaligned(base.add(pe) as *const u32) != 0x4550 {
        return None;
    }
    let optional = pe + 24;
    if std::ptr::read_unaligned(base.add(optional) as *const u16) != 0x20b {
        return None;
    }
    let size = std::ptr::read_unaligned(base.add(optional + 56) as *const u32) as usize;
    let read32 = |offset: usize| -> Option<u32> {
        if offset.checked_add(4)? > size {
            return None;
        }
        Some(std::ptr::read_unaligned(base.add(offset) as *const u32))
    };
    let imports = read32(optional + 120)? as usize;
    let import_size = read32(optional + 124)? as usize;
    if imports == 0 {
        return Some(0);
    }
    let mut count = 0;
    for descriptor in (imports..imports.checked_add(import_size)?).step_by(20) {
        let names = read32(descriptor)? as usize;
        let iat = read32(descriptor + 16)? as usize;
        if iat == 0 {
            break;
        }
        if names == 0 {
            continue;
        }
        let mut index = 0usize;
        loop {
            let offset = names.checked_add(index.checked_mul(8)?)?;
            if offset.checked_add(8)? > size {
                return None;
            }
            let name_rva = std::ptr::read_unaligned(base.add(offset) as *const u64);
            if name_rva == 0 {
                break;
            }
            if name_rva >> 63 == 0 {
                let start = usize::try_from(name_rva).ok()?.checked_add(2)?;
                let mut end = start;
                while end < size && *base.add(end) != 0 {
                    end += 1;
                }
                if end >= size {
                    return None;
                }
                // Borrow only the name bytes, never an immutable slice spanning the IAT we write.
                let replacement = match std::slice::from_raw_parts(base.add(start), end - start) {
                    b"RoGetActivationFactory" => winrt::activation_factory as *const () as usize,
                    b"GetCurrentPackageId" => package_id as *const () as usize,
                    b"GetCurrentPackageFullName" => full_name as *const () as usize,
                    b"GetCurrentPackageFamilyName" => family_name as *const () as usize,
                    b"GetCurrentPackagePath" => package_path as *const () as usize,
                    b"GetPackageFamilyName" => family_for_process as *const () as usize,
                    b"GetPackageFullName" => full_for_process as *const () as usize,
                    _ => 0,
                };
                if replacement != 0 {
                    let slot = iat.checked_add(index.checked_mul(8)?)?;
                    if slot.checked_add(8)? > size {
                        return None;
                    }
                    let address = base.add(slot) as *mut c_void;
                    if address as usize % std::mem::align_of::<usize>() != 0 {
                        return None;
                    }
                    let mut old = 0;
                    if VirtualProtect(address, 8, PAGE_READWRITE, &mut old) == 0 {
                        return None;
                    }
                    (address as *const std::sync::atomic::AtomicUsize)
                        .as_ref()?
                        .store(replacement, std::sync::atomic::Ordering::SeqCst);
                    let mut ignored = 0;
                    if VirtualProtect(address, 8, old, &mut ignored) == 0 {
                        return None;
                    }
                    count += 1;
                }
            }
            index += 1;
        }
    }
    Some(count)
}

unsafe extern "system" fn package_id(len: *mut u32, out: *mut u8) -> i32 {
    trace(b"GetCurrentPackageId\n");
    IDENTITY.get().unwrap().write_id(len, out)
}
unsafe extern "system" fn full_name(len: *mut u32, out: *mut u16) -> i32 {
    trace(b"GetCurrentPackageFullName\n");
    write_string(&IDENTITY.get().unwrap().full, len, out)
}
unsafe extern "system" fn family_name(len: *mut u32, out: *mut u16) -> i32 {
    trace(b"GetCurrentPackageFamilyName\n");
    write_string(&IDENTITY.get().unwrap().family, len, out)
}

/// Remote-thread entry point; zero means all hooks were installed successfully.
///
/// # Safety
/// `manifest` must point to a NUL-terminated UTF-16 path valid throughout the call.
#[no_mangle]
pub unsafe extern "system" fn InitializePackageIdentity(manifest: *mut c_void) -> u32 {
    std::panic::catch_unwind(|| initialize(manifest)).unwrap_or(4)
}

unsafe fn initialize(manifest: *mut c_void) -> u32 {
    use std::os::windows::ffi::OsStrExt;
    if let Some(path) = std::env::var_os("CODEX_IDENTITY_TRACE") {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = TRACE.set(file);
        }
    }
    trace(b"InitializePackageIdentity (native-addon scope)\n");
    if manifest.is_null() || IDENTITY.get().is_some() {
        return 1;
    }
    let ptr = manifest as *const u16;
    let mut len = 0;
    while len < 32768 && *ptr.add(len) != 0 {
        len += 1;
    }
    if len == 32768 {
        return 1;
    }
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
        ptr, len,
    )));
    let Ok(identity) = Identity::from_manifest(&path) else {
        return 2;
    };
    let Ok(exe) = std::env::current_exe() else {
        return 2;
    };
    let Some(root) = exe.parent() else {
        return 2;
    };
    let _ = ROOT.set(root.as_os_str().encode_wide().chain([0]).collect());
    if IDENTITY.set(identity).is_err() {
        return 1;
    }
    let kernel = GetModuleHandleW(codex_package_identity::wide("kernel32.dll").as_ptr());
    let Some(load) = GetProcAddress(kernel, c"LoadLibraryExW".as_ptr() as _) else {
        return 3;
    };
    let Some(simple) = GetProcAddress(kernel, c"LoadLibraryW".as_ptr() as _) else {
        return 3;
    };
    let Ok(load) = GenericDetour::new(
        std::mem::transmute::<unsafe extern "system" fn() -> isize, LoadFn>(load),
        load_library as LoadFn,
    ) else {
        return 3;
    };
    let Ok(simple) = GenericDetour::new(
        std::mem::transmute::<unsafe extern "system" fn() -> isize, LoadSimpleFn>(simple),
        load_simple as LoadSimpleFn,
    ) else {
        return 3;
    };
    let _ = LOADER.set(load);
    let _ = SIMPLE_LOADER.set(simple);
    if LOADER.get().unwrap().enable().is_err() {
        return 3;
    }
    if SIMPLE_LOADER.get().unwrap().enable().is_err() {
        let _ = LOADER.get().unwrap().disable();
        return 3;
    }
    0
}
