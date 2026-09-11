//! Per-user codex: handler, refreshed when launching or installing an app version.
use anyhow::{bail, Context, Result};
use std::path::Path;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Shell::{
    AssocQueryStringW, SHChangeNotify, ASSOCF_IS_PROTOCOL, ASSOCSTR_COMMAND, SHCNE_ASSOCCHANGED,
    SHCNF_IDLIST,
};

fn command(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

fn effective_command() -> Option<String> {
    let scheme = wide("codex");
    let verb = wide("open");
    let mut size = 0;
    unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_COMMAND,
            PCWSTR(scheme.as_ptr()),
            PCWSTR(verb.as_ptr()),
            PWSTR::null(),
            &mut size,
        )
        .ok()
        .ok()?;
        if size == 0 {
            return None;
        }
        let mut buffer = vec![0u16; size as usize];
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_COMMAND,
            PCWSTR(scheme.as_ptr()),
            PCWSTR(verb.as_ptr()),
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
        .ok()
        .ok()?;
        Some(String::from_utf16_lossy(
            &buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())],
        ))
    }
}

/// Leave Windows' protected UserChoice alone; verify the effective association
/// after registration so a Store/default-app override is reported, not hidden.
pub fn ensure(exe: &Path) -> Result<()> {
    if !exe.is_file() {
        bail!("codex: target does not exist: {}", exe.display());
    }
    let expected = command(exe);
    if effective_command().as_deref() == Some(expected.as_str()) {
        return Ok(());
    }
    write_key(
        r"Software\Classes\codex",
        &[("", "URL:Codex Protocol"), ("URL Protocol", "")],
        false,
    )?;
    write_key(r"Software\Classes\codex\shell", &[("", "open")], false)?;
    write_key(
        r"Software\Classes\codex\shell\open\command",
        &[("", &expected)],
        true,
    )?;
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    if effective_command().as_deref() != Some(expected.as_str()) {
        bail!("Windows still routes codex: links to a different handler. Select this Codex installation for the CODEX link type in Windows Settings > Apps > Default apps. Expected: {expected}");
    }
    Ok(())
}

fn write_key(path: &str, values: &[(&str, &str)], clear_delegate: bool) -> Result<()> {
    let path = wide(path);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()
        .context("opening codex: registration")?;
        let result = (|| -> Result<()> {
            for (name, value) in values {
                let name = wide(name);
                let value = wide(value);
                let bytes =
                    std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2);
                RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes))
                    .ok()
                    .context("writing codex: registration")?;
            }
            if clear_delegate {
                let delegate = wide("DelegateExecute");
                let result = RegDeleteValueW(key, PCWSTR(delegate.as_ptr()));
                if result != windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                    result.ok().context("removing stale codex: delegate")?;
                }
            }
            Ok(())
        })();
        let _ = RegCloseKey(key);
        result
    }
}

/// Registration errors must not prevent normal launching or invalidate a
/// completed update, but should be visible in this GUI-only application.
pub fn check_and_report(exe: &Path) {
    if let Err(error) = ensure(exe) {
        crate::dialogs::error(&format!(
            "Could not repair codex: links. The app can still launch normally.\n\n{error:#}"
        ));
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_executable_and_url_argument() {
        assert_eq!(
            command(Path::new(r"C:\Program Files\Codex\ChatGPT.exe")),
            r#""C:\Program Files\Codex\ChatGPT.exe" "%1""#
        );
        assert_eq!(
            command(Path::new(r"C:\Codex\Codex.exe")),
            r#""C:\Codex\Codex.exe" "%1""#
        );
    }
}
