//! Native MessageBox wrappers. Used where a full Slint screen would be
//! overkill (headless uninstall, mid-update confirmation).

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_SYSTEMMODAL,
    MB_YESNO,
};

pub fn yes_no(title: &str, body: &str) -> bool {
    let title_w = to_wide(title);
    let body_w = to_wide(body);
    unsafe {
        let result = MessageBoxW(
            HWND::default(),
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND | MB_SYSTEMMODAL,
        );
        result == IDYES
    }
}

pub fn error(body: &str) {
    let title_w = to_wide("Codex launcher");
    let body_w = to_wide(body);
    unsafe {
        let _ = MessageBoxW(
            HWND::default(),
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_SYSTEMMODAL,
        );
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
