//! x64 startup injection with an entry-point gate and an explicit initialization handshake.
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::{c_void, OsStr},
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        Diagnostics::{Debug::*, ToolHelp::*},
        LibraryLoader::*,
        Memory::*,
        Threading::*,
    },
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn check(ok: BOOL, action: &str) -> Result<()> {
    if ok == 0 {
        Err(std::io::Error::last_os_error()).with_context(|| action.to_owned())
    } else {
        Ok(())
    }
}
fn wide(s: &OsStr) -> Result<Vec<u16>> {
    let mut v: Vec<_> = s.encode_wide().collect();
    ensure!(!v.contains(&0), "embedded NUL in argument/path");
    v.push(0);
    Ok(v)
}

// Windows CommandLineToArgvW/CRT quoting, including empty arguments and trailing slashes.
fn command_line(exe: &Path, args: &[std::ffi::OsString]) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    for arg in std::iter::once(exe.as_os_str()).chain(args.iter().map(|a| a.as_os_str())) {
        if !out.is_empty() {
            out.push(32);
        }
        out.push(34);
        let mut slashes = 0;
        for ch in arg.encode_wide() {
            ensure!(ch != 0, "embedded NUL in argument");
            if ch == 92 {
                slashes += 1;
                continue;
            }
            out.extend(std::iter::repeat(92).take(if ch == 34 {
                slashes * 2 + 1
            } else {
                slashes
            }));
            out.push(ch);
            slashes = 0;
        }
        out.extend(std::iter::repeat(92).take(slashes * 2));
        out.push(34);
    }
    ensure!(out.len() < 32767, "command line exceeds Windows limit");
    out.push(0);
    Ok(out)
}

/// Starts only a newly created child; never attaches to an existing app.
/// On any setup failure the new child is terminated, rather than left suspended.
pub fn launch(exe: &Path, dll: &Path, manifest: &Path, args: &[std::ffi::OsString]) -> Result<u32> {
    ensure!(cfg!(target_arch = "x86_64"), "launcher requires x86_64");
    let exe = canonical_path(exe)?;
    let dll = canonical_path(dll)?;
    let manifest = canonical_path(manifest)?;
    codex_package_identity::Identity::from_manifest(&manifest)?;
    ensure_x64(&exe)?;
    ensure_x64(&dll)?;
    let app = wide(exe.as_os_str())?;
    let cwd = wide(
        exe.parent()
            .context("executable has no parent")?
            .as_os_str(),
    )?;
    let mut command = command_line(&exe, args)?;
    unsafe {
        let mut si: STARTUPINFOW = zeroed();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = zeroed();
        check(
            CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                0,
                DEBUG_ONLY_THIS_PROCESS,
                null(),
                cwd.as_ptr(),
                &si,
                &mut pi,
            ),
            "CreateProcessW",
        )?;
        let process = Handle(pi.hProcess);
        let thread = Handle(pi.hThread);
        let result = (|| {
            hold_at_entry(&pi)?;
            inject(&pi, &dll, &manifest)?;
            ensure!(
                ResumeThread(thread.0) != u32::MAX,
                "ResumeThread: {}",
                std::io::Error::last_os_error()
            );
            Ok(pi.dwProcessId)
        })();
        if result.is_err() {
            TerminateProcess(process.0, 1);
            DebugActiveProcessStop(pi.dwProcessId);
            WaitForSingleObject(process.0, 5000);
        }
        result
    }
}

// Chromium turns its executable path into a URL; a verbatim \\?\ prefix breaks that conversion.
fn canonical_path(path: &Path) -> Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let path = path.canonicalize()?;
    let chars: Vec<u16> = path.as_os_str().encode_wide().collect();
    let plain = if chars.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        [vec![92, 92], chars[8..].to_vec()].concat()
    } else if chars.starts_with(&[92, 92, 63, 92]) {
        chars[4..].to_vec()
    } else {
        chars
    };
    Ok(std::ffi::OsString::from_wide(&plain).into())
}

fn ensure_x64(path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let mut dos = [0; 64];
    file.read_exact(&mut dos)?;
    ensure!(&dos[..2] == b"MZ", "{} is not a PE file", path.display());
    file.seek(SeekFrom::Start(
        u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64,
    ))?;
    let mut pe = [0; 6];
    file.read_exact(&mut pe)?;
    ensure!(
        &pe[..4] == b"PE\0\0" && u16::from_le_bytes([pe[4], pe[5]]) == 0x8664,
        "{} is not an x64 PE image",
        path.display()
    );
    Ok(())
}

unsafe fn patch_byte(process: HANDLE, address: usize, value: u8) -> Result<()> {
    let mut protection = 0;
    check(
        VirtualProtectEx(
            process,
            address as _,
            1,
            PAGE_EXECUTE_READWRITE,
            &mut protection,
        ),
        "protect entry point",
    )?;
    let mut written = 0;
    let result = check(
        WriteProcessMemory(
            process,
            address as _,
            &value as *const _ as _,
            1,
            &mut written,
        ),
        "patch entry point",
    );
    let mut ignored = 0;
    let restored = check(
        VirtualProtectEx(process, address as _, 1, protection, &mut ignored),
        "restore entry protection",
    );
    result?;
    restored?;
    ensure!(written == 1, "partial entry point write");
    check(
        FlushInstructionCache(process, address as _, 1),
        "flush instruction cache",
    )
}

unsafe fn hold_at_entry(pi: &PROCESS_INFORMATION) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut entry = 0;
    let mut original = 0u8;
    loop {
        ensure!(
            Instant::now() < deadline,
            "timed out before executable entry point"
        );
        let mut event: DEBUG_EVENT = zeroed();
        if WaitForDebugEvent(&mut event, 100) == 0 {
            ensure!(
                GetLastError() == ERROR_SEM_TIMEOUT,
                "WaitForDebugEvent: {}",
                std::io::Error::last_os_error()
            );
            continue;
        }
        let mut status = DBG_CONTINUE;
        let mut reached = false;
        match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let info = event.u.CreateProcessInfo;
                if !info.hFile.is_null() {
                    CloseHandle(info.hFile);
                }
                // Debug-event process/thread handles are closed by Windows at detach.
                entry = info.lpStartAddress.context("missing PE entry point")? as usize;
                let mut read = 0;
                check(
                    ReadProcessMemory(
                        pi.hProcess,
                        entry as _,
                        &mut original as *mut _ as _,
                        1,
                        &mut read,
                    ),
                    "read entry point",
                )?;
                ensure!(
                    read == 1 && original != 0xcc,
                    "entry point already contains a breakpoint"
                );
                patch_byte(pi.hProcess, entry, 0xcc)?;
            }
            LOAD_DLL_DEBUG_EVENT => {
                let h = event.u.LoadDll.hFile;
                if !h.is_null() {
                    CloseHandle(h);
                }
            }
            EXCEPTION_DEBUG_EVENT => {
                let exception = event.u.Exception.ExceptionRecord;
                if exception.ExceptionCode == EXCEPTION_BREAKPOINT
                    && exception.ExceptionAddress as usize == entry
                {
                    ensure!(
                        event.dwThreadId == pi.dwThreadId,
                        "entry breakpoint on unexpected thread"
                    );
                    patch_byte(pi.hProcess, entry, original)?;
                    // Win64 requires a 16-byte-aligned CONTEXT. The generated
                    // binding's Rust alignment alone is not sufficient.
                    #[repr(C, align(16))]
                    struct AlignedContext(CONTEXT);
                    let mut context = AlignedContext(zeroed());
                    context.0.ContextFlags = CONTEXT_CONTROL_AMD64;
                    check(
                        GetThreadContext(pi.hThread, &mut context.0),
                        "get entry context",
                    )?;
                    context.0.Rip = entry as u64;
                    check(
                        SetThreadContext(pi.hThread, &context.0),
                        "restore entry context",
                    )?;
                    ensure!(
                        SuspendThread(pi.hThread) != u32::MAX,
                        "suspend entry thread failed"
                    );
                    reached = true;
                } else if exception.ExceptionCode != EXCEPTION_BREAKPOINT {
                    status = DBG_EXCEPTION_NOT_HANDLED;
                }
            }
            EXIT_PROCESS_DEBUG_EVENT => bail!(
                "child exited before entry point (code {})",
                event.u.ExitProcess.dwExitCode
            ),
            _ => {}
        }
        check(
            ContinueDebugEvent(event.dwProcessId, event.dwThreadId, status),
            "continue debug event",
        )?;
        if reached {
            check(
                DebugActiveProcessStop(pi.dwProcessId),
                "detach startup debugger",
            )?;
            return Ok(());
        }
    }
}

unsafe fn remote_module(pid: u32, basename: &OsStr) -> Result<usize> {
    let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
    ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        "module snapshot: {}",
        std::io::Error::last_os_error()
    );
    let snapshot = Handle(snapshot);
    let mut item: MODULEENTRY32W = zeroed();
    item.dwSize = size_of::<MODULEENTRY32W>() as u32;
    let mut ok = Module32FirstW(snapshot.0, &mut item);
    let expected = basename.to_string_lossy();
    while ok != 0 {
        let len = item
            .szModule
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(item.szModule.len());
        if String::from_utf16_lossy(&item.szModule[..len]).eq_ignore_ascii_case(&expected) {
            return Ok(item.modBaseAddr as usize);
        }
        ok = Module32NextW(snapshot.0, &mut item);
    }
    bail!("module {} is not loaded in child", expected)
}

unsafe fn remote_loader(pid: u32) -> Result<usize> {
    let kernel = GetModuleHandleW(codex_package_identity::wide("kernel32.dll").as_ptr());
    let address = GetProcAddress(kernel, c"LoadLibraryW".as_ptr() as _)
        .context("LoadLibraryW export missing")? as usize;
    let mut owner = null_mut();
    check(
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as _,
            &mut owner,
        ),
        "resolve forwarded LoadLibraryW owner",
    )?;
    let mut path = vec![0u16; 32768];
    let n = GetModuleFileNameW(owner, path.as_mut_ptr(), path.len() as u32);
    ensure!(
        n > 0 && n < path.len() as u32,
        "resolve LoadLibraryW module path failed"
    );
    use std::os::windows::ffi::OsStringExt;
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&path[..n as usize]));
    Ok(
        remote_module(pid, path.file_name().context("module basename missing")?)? + address
            - owner as usize,
    )
}

unsafe fn call_remote(process: HANDLE, address: usize, data: &[u16]) -> Result<u32> {
    let bytes = std::mem::size_of_val(data);
    let memory = VirtualAllocEx(
        process,
        null(),
        bytes,
        MEM_RESERVE | MEM_COMMIT,
        PAGE_READWRITE,
    );
    ensure!(
        !memory.is_null(),
        "VirtualAllocEx: {}",
        std::io::Error::last_os_error()
    );
    // On timeout the caller terminates the child; don't free a still-running thread's parameter.
    let mut written = 0;
    if let Err(e) = check(
        WriteProcessMemory(process, memory, data.as_ptr() as _, bytes, &mut written),
        "write remote parameter",
    ) {
        VirtualFreeEx(process, memory, 0, MEM_RELEASE);
        return Err(e);
    }
    if written != bytes {
        VirtualFreeEx(process, memory, 0, MEM_RELEASE);
        bail!("partial parameter write");
    }
    let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        usize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(address));
    let thread = CreateRemoteThread(process, null(), 0, start, memory, 0, null_mut());
    if thread.is_null() {
        let e = std::io::Error::last_os_error();
        VirtualFreeEx(process, memory, 0, MEM_RELEASE);
        return Err(e).context("CreateRemoteThread");
    }
    let thread = Handle(thread);
    ensure!(
        WaitForSingleObject(thread.0, 15000) == WAIT_OBJECT_0,
        "remote initialization failed or timed out"
    );
    let mut code = 0;
    let result = check(
        GetExitCodeThread(thread.0, &mut code),
        "remote thread result",
    );
    VirtualFreeEx(process, memory, 0, MEM_RELEASE);
    result?;
    Ok(code)
}

unsafe fn inject(pi: &PROCESS_INFORMATION, dll: &Path, manifest: &Path) -> Result<()> {
    call_remote(
        pi.hProcess,
        remote_loader(pi.dwProcessId)?,
        &wide(dll.as_os_str())?,
    )?;
    // LoadLibrary returns a pointer, but a thread exit code is only DWORD. Use the module list.
    let remote = remote_module(
        pi.dwProcessId,
        dll.file_name().context("DLL basename missing")?,
    )?;
    // Our DLL has no hook-installing DllMain; loading locally only resolves its export RVA.
    let local = LoadLibraryExW(
        wide(dll.as_os_str())?.as_ptr(),
        null_mut(),
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
    );
    ensure!(
        !local.is_null(),
        "load shim locally: {}",
        std::io::Error::last_os_error()
    );
    let export = GetProcAddress(local, c"InitializePackageIdentity".as_ptr() as _);
    let rva = export.map(|f| f as usize - local as usize);
    FreeLibrary(local);
    let code = call_remote(
        pi.hProcess,
        remote + rva.context("shim initialization export missing")?,
        &wide(manifest.as_os_str())?,
    )?;
    ensure!(code == 0, "identity shim initialization failed (code {code}; 1=argument/state, 2=manifest, 3=detour, 4=panic)");
    Ok(())
}
