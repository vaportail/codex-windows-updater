//! Native Win32 splash window. Shown at the top of proxy startup so the
//! user has visual feedback while Slint warms up and the bg update check
//! runs. Single dedicated thread with its own message loop; main thread
//! posts WM_CLOSE on drop to tear it down.

use std::cell::Cell;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, UpdateWindow, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, FW_BOLD, FW_NORMAL, HBRUSH, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DispatchMessageW, DrawIconEx, GetMessageW,
    GetSystemMetrics, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    DI_NORMAL, HICON, IDC_ARROW, LWA_ALPHA, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CLOSE,
    WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_TOPMOST,
    WS_POPUP,
};

const CLASS_NAME: PCWSTR = w!("CodexUpdaterSplash");
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 16;

const LOGICAL_W: i32 = 380;
const LOGICAL_H: i32 = 220;
const ICON_SIZE: i32 = 80;
const BAR_W: i32 = 240;
const BAR_H: i32 = 6;
const BAR_FG_W: i32 = 80;

// COLORREF byte order is 0x00BBGGRR.
const COLOR_BG: u32 = 0x002e1e1e; // #1e1e2e
const COLOR_TEXT: u32 = 0x00f4d6cd; // #cdd6f4
const COLOR_DIM: u32 = 0x00c8ada6; // #a6adc8
const COLOR_BAR_BG: u32 = 0x00443231; // #313244
const COLOR_BAR_FG: u32 = 0x00fab489; // #89b4fa

pub struct Splash {
    hwnd: usize,
}

// HWND messaging via PostMessageW is documented thread-safe.
unsafe impl Send for Splash {}

impl Splash {
    /// Spawn a splash on a dedicated thread. Returns once CreateWindowEx
    /// completes (HWND back via channel). `codex_exe`, if Some, is the
    /// path to load the logo icon from. Returns None if the window failed.
    pub fn show(codex_exe: Option<PathBuf>) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || unsafe {
            run_splash(codex_exe.as_deref(), tx);
        });
        let hwnd = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .ok()
            .filter(|h| *h != 0)?;
        Some(Self { hwnd })
    }
}

impl Drop for Splash {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            unsafe {
                let _ = PostMessageW(HWND(self.hwnd as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

// Per-thread state — splash thread only.
thread_local! {
    static HICON_PTR: Cell<isize> = const { Cell::new(0) };
    static BAR_OFFSET: Cell<i32> = const { Cell::new(0) };
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
}

unsafe fn run_splash(codex_exe: Option<&Path>, tx: mpsc::Sender<usize>) {
    let hinstance = match GetModuleHandleW(None) {
        Ok(h) => h,
        Err(_) => {
            let _ = tx.send(0);
            return;
        }
    };
    let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();

    let hinstance: HINSTANCE = HINSTANCE(hinstance.0);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        lpszClassName: CLASS_NAME,
        hCursor: cursor,
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        ..Default::default()
    };
    let _ = RegisterClassExW(&wc);

    let scale = GetDpiForSystem() as f32 / 96.0;
    SCALE.with(|s| s.set(scale));
    let w = (LOGICAL_W as f32 * scale) as i32;
    let h = (LOGICAL_H as f32 * scale) as i32;
    let screen_w = GetSystemMetrics(SM_CXSCREEN);
    let screen_h = GetSystemMetrics(SM_CYSCREEN);
    let x = ((screen_w - w) / 2).max(0);
    let y = ((screen_h - h) / 2).max(0);

    let hicon = codex_exe.and_then(|p| load_icon(p));
    if let Some(icon) = hicon {
        HICON_PTR.with(|c| c.set(icon.0 as isize));
    }

    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_APPWINDOW | WS_EX_TOPMOST,
        CLASS_NAME,
        w!("Codex"),
        WS_POPUP,
        x,
        y,
        w,
        h,
        HWND::default(),
        None,
        hinstance,
        None,
    );
    let hwnd = match hwnd {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            let _ = tx.send(0);
            return;
        }
    };

    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = UpdateWindow(hwnd);
    let _ = SetTimer(hwnd, TIMER_ID, TIMER_MS, None);

    let _ = tx.send(hwnd.0 as usize);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, HWND::default(), 0, 0).into() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if let Some(icon) = hicon {
        let _ = DestroyIcon(icon);
    }
    HICON_PTR.with(|c| c.set(0));
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            BAR_OFFSET.with(|c| c.set((c.get() + 6) % (LOGICAL_W * 3)));
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_ID);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let scale = SCALE.with(|s| s.get());
    let scaled = |v: i32| (v as f32 * scale) as i32;
    let w = scaled(LOGICAL_W);
    let h = scaled(LOGICAL_H);
    let icon_sz = scaled(ICON_SIZE);
    let bar_w = scaled(BAR_W);
    let bar_h = scaled(BAR_H);
    let bar_fg_w = scaled(BAR_FG_W);

    // Background.
    let bg_brush = CreateSolidBrush(COLORREF(COLOR_BG));
    let rect = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    FillRect(hdc, &rect, bg_brush);
    let _ = DeleteObject(HGDIOBJ(bg_brush.0));

    // Icon.
    let icon_y = scaled(24);
    let icon_x = (w - icon_sz) / 2;
    let icon_raw = HICON_PTR.with(|c| c.get());
    if icon_raw != 0 {
        let _ = DrawIconEx(
            hdc,
            icon_x,
            icon_y,
            HICON(icon_raw as *mut _),
            icon_sz,
            icon_sz,
            0,
            None,
            DI_NORMAL,
        );
    }

    SetBkMode(hdc, TRANSPARENT);

    // "Codex" wordmark.
    let title_y = icon_y + icon_sz + scaled(10);
    let title_h = scaled(28);
    let title_font = CreateFontW(
        scaled(22),
        0,
        0,
        0,
        FW_BOLD.0 as i32,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        w!("Segoe UI"),
    );
    let prev_font = SelectObject(hdc, HGDIOBJ(title_font.0));
    SetTextColor(hdc, COLORREF(COLOR_TEXT));
    let mut title: Vec<u16> = "Codex".encode_utf16().collect();
    let mut title_rect = RECT {
        left: 0,
        top: title_y,
        right: w,
        bottom: title_y + title_h,
    };
    DrawTextW(
        hdc,
        &mut title,
        &mut title_rect,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );

    // "Checking for updates…"
    let sub_y = title_y + title_h + scaled(2);
    let sub_h = scaled(18);
    let sub_font = CreateFontW(
        scaled(13),
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        w!("Segoe UI"),
    );
    SelectObject(hdc, HGDIOBJ(sub_font.0));
    SetTextColor(hdc, COLORREF(COLOR_DIM));
    let mut sub: Vec<u16> = "Checking for updates…".encode_utf16().collect();
    let mut sub_rect = RECT {
        left: 0,
        top: sub_y,
        right: w,
        bottom: sub_y + sub_h,
    };
    DrawTextW(
        hdc,
        &mut sub,
        &mut sub_rect,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );
    SelectObject(hdc, prev_font);

    // Marquee bar.
    let bar_y = sub_y + sub_h + scaled(14);
    let bar_x = (w - bar_w) / 2;
    let bar_bg = CreateSolidBrush(COLORREF(COLOR_BAR_BG));
    let bar_rect = RECT {
        left: bar_x,
        top: bar_y,
        right: bar_x + bar_w,
        bottom: bar_y + bar_h,
    };
    FillRect(hdc, &bar_rect, bar_bg);
    let _ = DeleteObject(HGDIOBJ(bar_bg.0));

    // Sliding highlight: cycles from -BAR_FG_W to BAR_W, then wraps.
    let raw = BAR_OFFSET.with(|c| c.get());
    let span = BAR_W + BAR_FG_W;
    let local = raw % span - BAR_FG_W;
    let pos = scaled(local);
    let fg_left = (bar_x + pos).max(bar_x);
    let fg_right = (bar_x + pos + bar_fg_w).min(bar_x + bar_w);
    if fg_right > fg_left {
        let bar_fg = CreateSolidBrush(COLORREF(COLOR_BAR_FG));
        let fg_rect = RECT {
            left: fg_left,
            top: bar_y,
            right: fg_right,
            bottom: bar_y + bar_h,
        };
        FillRect(hdc, &fg_rect, bar_fg);
        let _ = DeleteObject(HGDIOBJ(bar_fg.0));
    }

    let _ = DeleteObject(HGDIOBJ(title_font.0));
    let _ = DeleteObject(HGDIOBJ(sub_font.0));

    let _ = EndPaint(hwnd, &ps);
}

/// Best-effort: extract the largest icon group from `path`. Returns the
/// system-large variant; DrawIconEx scales it to our target size.
unsafe fn load_icon(path: &Path) -> Option<HICON> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut large = HICON::default();
    let n = ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut large), None, 1);
    if n > 0 && !large.is_invalid() {
        Some(large)
    } else {
        None
    }
}
