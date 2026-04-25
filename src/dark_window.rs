//! Eliminate the white flash that appears between when Slint shows its native
//! window and when its renderer produces the first visible frame.
//!
//! Approach:
//!   1. put a tiny native black cover window above the future Slint bounds
//!      immediately before `ui.show()`, then remove it after first render.
//!      This is intentionally boring: if the platform flashes before Slint
//!      paints, the user sees black, not white;
//!   2. install a thread-local CBT hook before the Slint AppWindow is created.
//!      HCBT_CREATEWND fires synchronously during `CreateWindowExW`, before the
//!      OS paints the window. For the real AppWindow, we swap the class
//!      background brush, subclass WM_ERASEBKGND to fill dark, and flip on DWM
//!      immersive dark mode so the title bar starts dark too.
//!
//! Only applies to top-level windows titled "Codex Updater" on the thread that
//! called `install` — i.e. the main UI thread. Native dialogs keep their system
//! look. The splash window runs on its own thread and is not affected (it paints
//! itself).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, UpdateWindow, HBRUSH, HDC, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowRect,
    RegisterClassExW, SetClassLongPtrW, SetWindowsHookExW, ShowWindow, CBT_CREATEWNDW,
    GCLP_HBRBACKGROUND, HCBT_CREATEWND, HHOOK, SW_SHOW, WH_CBT, WM_ERASEBKGND, WM_PAINT,
    WNDCLASSEXW, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

// Matches AppWindow.background `#1e1e2e`. COLORREF byte order is 0x00BBGGRR.
const COLOR_BG: u32 = 0x002e1e1e;
const SUBCLASS_ID: usize = 0xC0DE_2026;
const ENABLE_COVER: bool = true;
const COVER_CLASS: PCWSTR = w!("CodexUpdaterDarkCover");

static INSTALLED: AtomicBool = AtomicBool::new(false);
static COVER_REGISTERED: AtomicBool = AtomicBool::new(false);
static DARK_BRUSH: AtomicIsize = AtomicIsize::new(0);
static COVER_HWND: AtomicIsize = AtomicIsize::new(0);
static APP_HWND: AtomicIsize = AtomicIsize::new(0);

/// Install the hook immediately before the AppWindow is created. Idempotent —
/// subsequent calls are no-ops. Must be called *after* any pre-window dialogs
/// (e.g. `prompt_kill_codex_for` on the auto-update path), so only Slint's
/// AppWindow is eligible for dark pre-paint setup.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let brush = CreateSolidBrush(COLORREF(COLOR_BG));
        DARK_BRUSH.store(brush.0 as isize, Ordering::SeqCst);

        // Thread-local hook — only fires for windows created on this thread.
        // Returning the HHOOK leaks; the OS reclaims at process exit.
        let _ = SetWindowsHookExW(
            WH_CBT,
            Some(cbt_proc),
            HINSTANCE::default(),
            GetCurrentThreadId(),
        );
    }
}

/// Show a native black cover over the area where the Slint window will appear.
/// This runs immediately before `ui.show()` to mask any compositor-white frame
/// that Slint/Winit may emit during first presentation.
pub fn show_cover(ui: &crate::AppWindow) {
    use slint::ComponentHandle;

    if !ENABLE_COVER {
        return;
    }
    if COVER_HWND.load(Ordering::SeqCst) != 0 {
        return;
    }
    let rect = app_window_rect().or_else(|| {
        let position = ui.window().position();
        let size = ui.window().size();
        Some((
            position.x,
            position.y,
            size.width as i32,
            size.height as i32,
        ))
    });
    unsafe {
        let Some((x, y, w, h)) = rect else { return };
        let Some(hwnd) = create_cover(x, y, w, h) else {
            return;
        };
        COVER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
    }
}

/// Remove the cover after Slint has produced a frame. If the backend does not
/// support render notifications, use a short fallback timer.
pub fn hide_cover_after_first_render(ui: &crate::AppWindow) {
    use slint::ComponentHandle;

    let result = ui.window().set_rendering_notifier(|state, _graphics_api| {
        if matches!(state, slint::RenderingState::AfterRendering) {
            hide_cover();
        }
    });
    if result.is_err() {
        slint::Timer::single_shot(std::time::Duration::from_millis(180), hide_cover);
    }
}

unsafe extern "system" fn cbt_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HCBT_CREATEWND as i32 {
        let hwnd = HWND(wparam.0 as *mut _);
        if !hwnd.is_invalid() && is_app_window_create(lparam) {
            darken_window(hwnd);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

unsafe fn is_app_window_create(lparam: LPARAM) -> bool {
    let create = lparam.0 as *const CBT_CREATEWNDW;
    if create.is_null() || (*create).lpcs.is_null() {
        return false;
    }

    let cs = &*(*create).lpcs;
    if !cs.hwndParent.0.is_null() || (cs.style as u32 & WS_CHILD.0) != 0 {
        return false;
    }

    wide_eq(cs.lpszName.as_ptr(), "Codex Updater")
}

unsafe fn darken_window(hwnd: HWND) {
    APP_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

    let brush = DARK_BRUSH.load(Ordering::SeqCst);
    if brush != 0 {
        // Dark fill for any class-background erase path.
        let _ = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush);
        // Dark fill for Winit's zero-background class path. This catches the
        // first WM_ERASEBKGND that can happen before Slint renders a frame.
        let _ = SetWindowSubclass(hwnd, Some(dark_subclass_proc), SUBCLASS_ID, brush as usize);
    }

    let dark = BOOL::from(true);
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark as *const _ as *const _,
        std::mem::size_of::<BOOL>() as u32,
    );
}

fn app_window_rect() -> Option<(i32, i32, i32, i32)> {
    let hwnd = HWND(APP_HWND.load(Ordering::SeqCst) as *mut _);
    if hwnd.is_invalid() {
        return None;
    }

    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some((
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        ))
    }
}

fn hide_cover() {
    let hwnd = COVER_HWND.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = DestroyWindow(HWND(hwnd as *mut _));
    }
}

unsafe fn create_cover(x: i32, y: i32, w: i32, h: i32) -> Option<HWND> {
    let module = GetModuleHandleW(None).ok()?;
    let hinstance = HINSTANCE(module.0);

    if !COVER_REGISTERED.swap(true, Ordering::SeqCst) {
        let brush = HBRUSH(DARK_BRUSH.load(Ordering::SeqCst) as *mut _);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(cover_wnd_proc),
            hInstance: hinstance,
            lpszClassName: COVER_CLASS,
            hbrBackground: brush,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }

    match CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        COVER_CLASS,
        w!(""),
        WS_POPUP,
        x,
        y,
        w,
        h,
        HWND::default(),
        None,
        hinstance,
        None,
    ) {
        Ok(hwnd) if !hwnd.is_invalid() => Some(hwnd),
        _ => None,
    }
}

unsafe extern "system" fn cover_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_ok() {
                let brush = HBRUSH(DARK_BRUSH.load(Ordering::SeqCst) as *mut _);
                let _ = FillRect(hdc, &rect, brush);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn dark_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    brush: usize,
) -> LRESULT {
    if msg == WM_ERASEBKGND && brush != 0 {
        let hdc = HDC(wparam.0 as *mut _);
        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_ok() {
            let _ = FillRect(hdc, &rect, HBRUSH(brush as *mut _));
            return LRESULT(1);
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

unsafe fn wide_eq(mut ptr: *const u16, expected: &str) -> bool {
    if ptr.is_null() {
        return false;
    }

    for expected_unit in expected.encode_utf16() {
        if *ptr != expected_unit {
            return false;
        }
        ptr = ptr.add(1);
    }
    *ptr == 0
}
