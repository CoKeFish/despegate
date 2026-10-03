//! The on-screen side of despegate: the full-screen lock and the banner.
//!
//! Everything here runs on one UI thread of the agent. The shared [`UiState`]
//! says what should be visible; a timer on this thread makes the windows
//! match it and keeps the lock on top of everything else.

use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{Local, NaiveDateTime};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, EnumDisplayMonitors, FillRect, GetMonitorInfoW, HDC, HMONITOR,
    InvalidateRect, MONITORINFO, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SetFocus, VK_APPS, VK_CONTROL, VK_ESCAPE, VK_F4, VK_LWIN, VK_RWIN, VK_SPACE,
    VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetClientRect, GetForegroundWindow, GetMessageW, GetWindowLongPtrW,
    GetWindowThreadProcessId, HC_ACTION, HHOOK, HWND_MESSAGE, HWND_TOPMOST, IDC_ARROW,
    KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LoadCursorW, MONITORINFOF_PRIMARY, MSG, RegisterClassExW,
    SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, ShowWindow,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_CHAR, WM_CLOSE, WM_ERASEBKGND,
    WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::i18n::Lang;
use crate::paths::Paths;
use crate::web;
use crate::{tr, wide};

/// What the UI thread draws. The agent keeps it in step with the daemon.
#[derive(Clone, Default, PartialEq)]
pub struct UiState {
    pub mode: Mode,
    pub lang: Lang,
    /// "light", "dark" or "system".
    pub theme: String,
    /// What the user has typed so far towards the emergency challenge.
    pub typed: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Idle,
    Banner {
        text: String,
    },
    Lock(LockView),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LockView {
    pub until: NaiveDateTime,
    pub reasons: String,
    /// Photos and videos, as stored names in the media directory.
    #[serde(default)]
    pub media: Vec<String>,
    pub challenge: Option<String>,
    pub emergency_minutes: u32,
}

/// "1 h 05 min" or "4:59", for banners and the lock screen.
pub fn countdown(left: chrono::Duration) -> String {
    let seconds = left.num_seconds().max(0);
    if seconds >= 3600 {
        format!("{} h {:02} min", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

const REFRESH_MS: u32 = 200;

const KIND_LOCK_PRIMARY: isize = 1;
const KIND_LOCK_SECONDARY: isize = 2;
const KIND_BANNER: isize = 3;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// The same palette as the lock page, for what is drawn by hand: the dark
/// screen the sticker sits on, which is also what the other monitors show.
struct Palette {
    background: COLORREF,
    title: COLORREF,
    text: COLORREF,
    muted: COLORREF,
    good: COLORREF,
    bad: COLORREF,
}

const LIGHT: Palette = Palette {
    background: rgb(29, 27, 69),
    title: rgb(245, 184, 0),
    text: rgb(238, 237, 251),
    muted: rgb(166, 164, 203),
    good: rgb(245, 184, 0),
    bad: rgb(255, 159, 140),
};
const DARK: Palette = Palette {
    background: rgb(11, 10, 31),
    ..LIGHT
};
const BANNER_BACKGROUND: COLORREF = rgb(245, 184, 0);
const BANNER_TEXT: COLORREF = rgb(29, 27, 69);

fn palette(theme: &str) -> &'static Palette {
    if web::is_dark(theme) { &DARK } else { &LIGHT }
}

#[derive(Clone, Copy, PartialEq)]
struct Monitor {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    primary: bool,
}

/// The windows currently on screen. Only touched from the UI thread's timer.
struct Windows {
    monitors: Vec<Monitor>,
    /// One per monitor, the primary monitor's first.
    lock: Vec<HWND>,
    /// The lock screen page inside the primary lock window, and what it shows.
    /// Without it (no WebView2 runtime) the lock screen is drawn by hand.
    web: Option<wry::WebView>,
    web_for: Option<LockView>,
    banner: HWND,
    hook: HHOOK,
}

impl Default for Windows {
    fn default() -> Self {
        Windows {
            monitors: Vec::new(),
            lock: Vec::new(),
            web: None,
            web_for: None,
            banner: null_mut(),
            hook: null_mut(),
        }
    }
}

static UI: OnceLock<Arc<Mutex<UiState>>> = OnceLock::new();
static PATHS: OnceLock<Paths> = OnceLock::new();

thread_local! {
    static WINDOWS: RefCell<Windows> = RefCell::default();
    /// Creating a web view pumps messages, which would re-enter the timer.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

fn snapshot() -> UiState {
    UI.get()
        .map(|ui| ui.lock().unwrap().clone())
        .unwrap_or_default()
}

/// Runs the UI message loop. Does not return while the process lives.
pub fn run(ui: Arc<Mutex<UiState>>, paths: Paths) {
    let _ = UI.set(ui);
    let _ = PATHS.set(paths);
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        let class = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: null_mut(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        RegisterClassExW(&wc);
        let controller = CreateWindowExW(
            0,
            class.as_ptr(),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            instance,
            null(),
        );
        SetTimer(controller, 1, REFRESH_MS, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

const CLASS: &str = "despegate-overlay";

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            if BUSY.replace(true) {
                return 0;
            }
            // The window procedure is re-entered while windows are created and
            // destroyed, so the bookkeeping is taken out of the cell meanwhile.
            let mut windows = WINDOWS.take();
            unsafe { reconcile(&mut windows) };
            WINDOWS.set(windows);
            BUSY.set(false);
            0
        }
        WM_PAINT => {
            unsafe { paint(hwnd) };
            0
        }
        WM_ERASEBKGND => 1,
        WM_CHAR => {
            if unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } == KIND_LOCK_PRIMARY {
                on_char(wparam as u32);
                unsafe { InvalidateRect(hwnd, null(), 0) };
            }
            0
        }
        // Alt+F4 and friends must not close the overlay.
        WM_CLOSE => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Makes the windows on screen match the mode the daemon asked for.
unsafe fn reconcile(windows: &mut Windows) {
    unsafe {
        let state = snapshot();
        match state.mode {
            Mode::Lock(view) => {
                destroy_banner(windows);
                show_lock(windows, &view, state.lang, &state.theme);
            }
            Mode::Banner { .. } => {
                destroy_lock(windows);
                show_banner(windows);
            }
            Mode::Idle => {
                destroy_lock(windows);
                destroy_banner(windows);
            }
        }
    }
}

unsafe fn create_window(
    kind: isize,
    ex_style: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> HWND {
    unsafe {
        let hwnd = CreateWindowExW(
            ex_style | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            wide(CLASS).as_ptr(),
            wide("despegate").as_ptr(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, kind);
        hwnd
    }
}

unsafe fn show_lock(windows: &mut Windows, view: &LockView, lang: Lang, theme: &str) {
    unsafe {
        let monitors = monitors();
        if windows.lock.is_empty() || monitors != windows.monitors {
            destroy_lock(windows);
            for (i, m) in monitors.iter().enumerate() {
                let kind = if i == 0 {
                    KIND_LOCK_PRIMARY
                } else {
                    KIND_LOCK_SECONDARY
                };
                let hwnd = create_window(kind, 0, m.left, m.top, m.width, m.height);
                ShowWindow(hwnd, SW_SHOW);
                windows.lock.push(hwnd);
            }
            windows.monitors = monitors;
            windows.hook = SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(null()),
                0,
            );
        }
        for hwnd in &windows.lock {
            SetWindowPos(
                *hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            InvalidateRect(*hwnd, null(), 0);
        }
        let Some(primary) = windows.lock.first().copied() else {
            return;
        };
        // The page shows one lock session; a new challenge or end time means a new page.
        if windows.web_for.as_ref() != Some(view) {
            windows.web = None;
            windows.web_for = Some(view.clone());
            if let (Some(paths), Some(ui), Some(m)) =
                (PATHS.get(), UI.get(), windows.monitors.first())
            {
                windows.web = web::build_lock(
                    primary,
                    (m.width, m.height),
                    view,
                    lang,
                    theme,
                    paths,
                    ui.clone(),
                )
                .ok();
            }
        }
        if take_foreground(primary)
            && let Some(web) = &windows.web
        {
            let _ = web.focus();
        }
    }
}

unsafe fn destroy_lock(windows: &mut Windows) {
    unsafe {
        // The page goes before the window that holds it.
        windows.web = None;
        windows.web_for = None;
        for hwnd in windows.lock.drain(..) {
            DestroyWindow(hwnd);
        }
        if !windows.hook.is_null() {
            UnhookWindowsHookEx(windows.hook);
            windows.hook = null_mut();
        }
    }
}

unsafe fn show_banner(windows: &mut Windows) {
    unsafe {
        if windows.banner.is_null() {
            let Some(m) = monitors().first().copied() else {
                return;
            };
            let width = m.width * 2 / 5;
            let height = m.height / 18;
            windows.banner = create_window(
                KIND_BANNER,
                WS_EX_NOACTIVATE,
                m.left + (m.width - width) / 2,
                m.top,
                width,
                height,
            );
            ShowWindow(windows.banner, SW_SHOWNOACTIVATE);
        }
        SetWindowPos(
            windows.banner,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        InvalidateRect(windows.banner, null(), 0);
    }
}

unsafe fn destroy_banner(windows: &mut Windows) {
    if !windows.banner.is_null() {
        unsafe { DestroyWindow(windows.banner) };
        windows.banner = null_mut();
    }
}

/// Windows refuses to hand the foreground to a background process unless its
/// input queue is attached to the current foreground thread's.
/// Returns whether the foreground had to be taken.
unsafe fn take_foreground(hwnd: HWND) -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground == hwnd {
            return false;
        }
        let me = GetCurrentThreadId();
        let other = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, null_mut())
        };
        let attached = other != 0 && other != me && AttachThreadInput(me, other, 1) != 0;
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        if attached {
            AttachThreadInput(me, other, 0);
        }
        true
    }
}

/// All monitors, the primary one first.
unsafe fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        unsafe {
            let list = &mut *(data as *mut Vec<Monitor>);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) != 0 {
                let rc = info.rcMonitor;
                list.push(Monitor {
                    left: rc.left,
                    top: rc.top,
                    width: rc.right - rc.left,
                    height: rc.bottom - rc.top,
                    primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
            1
        }
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            &mut list as *mut Vec<Monitor> as LPARAM,
        );
    }
    list.sort_by_key(|m| !m.primary);
    list
}

/// Swallows the keys that would let the user leave the lock screen.
/// (Ctrl+Alt+Del cannot be intercepted; Task Manager is closed by the daemon.)
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let key = &*(lparam as *const KBDLLHOOKSTRUCT);
            let vk = key.vkCode as u16;
            let alt = key.flags & LLKHF_ALTDOWN != 0;
            let ctrl = GetAsyncKeyState(VK_CONTROL as i32) < 0;
            let escape_route = matches!(vk, VK_LWIN | VK_RWIN | VK_APPS)
                || (alt && matches!(vk, VK_TAB | VK_ESCAPE | VK_F4 | VK_SPACE))
                || (ctrl && vk == VK_ESCAPE);
            if escape_route {
                return 1;
            }
        }
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }
}

/// Typing on the lock screen works towards the emergency challenge.
fn on_char(code: u32) {
    if let Some(ui) = UI.get() {
        ui.lock().unwrap().type_char(code);
    }
}

impl UiState {
    /// Feeds one character, as delivered by `WM_CHAR`, to the challenge. The
    /// daemon, not the UI, decides when what was typed is right.
    fn type_char(&mut self, code: u32) {
        const BACKSPACE: u32 = 8;
        let Mode::Lock(LockView {
            challenge: Some(challenge),
            ..
        }) = &self.mode
        else {
            return;
        };
        let room = self.typed.chars().count() < challenge.chars().count();
        match char::from_u32(code) {
            Some(_) if code == BACKSPACE => {
                self.typed.pop();
            }
            Some(c) if !c.is_control() && room => self.typed.extend(c.to_lowercase()),
            _ => {}
        }
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rc);

        // Draw off-screen and copy in one go so the countdown does not flicker.
        let buffer = CreateCompatibleDC(hdc);
        let bitmap = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
        let previous = SelectObject(buffer, bitmap);
        SetBkMode(buffer, TRANSPARENT as i32);

        let state = snapshot();
        match (GetWindowLongPtrW(hwnd, GWLP_USERDATA), &state.mode) {
            (KIND_LOCK_PRIMARY, Mode::Lock(view)) => draw_lock(
                buffer,
                rc,
                view,
                &state.typed,
                state.lang,
                palette(&state.theme),
            ),
            (KIND_BANNER, Mode::Banner { text }) => {
                fill(buffer, rc, BANNER_BACKGROUND);
                let style = Text {
                    size: rc.bottom * 2 / 5,
                    weight: 600,
                    face: "Segoe UI",
                    color: BANNER_TEXT,
                };
                draw_text(
                    buffer,
                    text,
                    rc,
                    &style,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
            _ => fill(buffer, rc, palette(&state.theme).background),
        }

        BitBlt(hdc, 0, 0, rc.right, rc.bottom, buffer, 0, 0, SRCCOPY);
        SelectObject(buffer, previous);
        DeleteObject(bitmap);
        DeleteDC(buffer);
        EndPaint(hwnd, &ps);
    }
}

struct Text {
    size: i32,
    weight: i32,
    face: &'static str,
    color: COLORREF,
}

unsafe fn fill(hdc: HDC, rc: RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, &rc, brush);
        DeleteObject(brush);
    }
}

unsafe fn draw_text(hdc: HDC, text: &str, mut rc: RECT, style: &Text, format: u32) {
    const DEFAULT_CHARSET: u32 = 1;
    const CLEARTYPE_QUALITY: u32 = 5;
    // DrawTextW reads the buffer even when told it holds zero characters, and
    // an empty Vec has no buffer to read.
    if text.is_empty() {
        return;
    }
    unsafe {
        let font = CreateFontW(
            style.size,
            0,
            0,
            0,
            style.weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            0,
            0,
            CLEARTYPE_QUALITY,
            0,
            wide(style.face).as_ptr(),
        );
        let previous = SelectObject(hdc, font);
        SetTextColor(hdc, style.color);
        let text = wide(text);
        DrawTextW(hdc, text.as_ptr(), text.len() as i32 - 1, &mut rc, format);
        SelectObject(hdc, previous);
        DeleteObject(font);
    }
}

/// The lock screen is laid out in fractions of the window height so it looks
/// the same at any resolution and DPI.
unsafe fn draw_lock(hdc: HDC, rc: RECT, view: &LockView, typed: &str, lang: Lang, p: &Palette) {
    let (w, h) = (rc.right, rc.bottom);
    let band = |from: f32, to: f32, margin: f32| RECT {
        left: (w as f32 * margin) as i32,
        top: (h as f32 * from) as i32,
        right: (w as f32 * (1.0 - margin)) as i32,
        bottom: (h as f32 * to) as i32,
    };
    let line = DT_CENTER | DT_SINGLELINE | DT_NOPREFIX;
    let wrapped = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
    let sans = |size: i32, weight: i32, color: COLORREF| Text {
        size,
        weight,
        face: "Segoe UI",
        color,
    };
    let mono = |color: COLORREF| Text {
        size: h / 38,
        weight: 400,
        face: "Consolas",
        color,
    };

    let left = view.until - Local::now().naive_local();
    let subtitle = tr!(
        lang,
        "lock.subtitle",
        until = view.until.format("%H:%M"),
        left = countdown(left)
    );
    let reasons = if view.reasons.is_empty() {
        tr!(lang, "lock.no_reasons")
    } else {
        view.reasons.clone()
    };

    unsafe {
        fill(hdc, rc, p.background);
        draw_text(
            hdc,
            &tr!(lang, "lock.title"),
            band(0.09, 0.24, 0.05),
            &sans(h / 8, 700, p.title),
            line,
        );
        draw_text(
            hdc,
            &subtitle,
            band(0.25, 0.31, 0.05),
            &sans(h / 30, 400, p.muted),
            line,
        );
        draw_text(
            hdc,
            &tr!(lang, "lock.why"),
            band(0.37, 0.41, 0.05),
            &sans(h / 52, 600, p.muted),
            line,
        );
        draw_text(
            hdc,
            &reasons,
            band(0.42, 0.74, 0.16),
            &sans(h / 26, 400, p.text),
            wrapped,
        );

        if let Some(challenge) = &view.challenge {
            let prompt = tr!(lang, "lock.emergency", minutes = view.emergency_minutes);
            draw_text(
                hdc,
                &prompt,
                band(0.77, 0.80, 0.05),
                &sans(h / 52, 400, p.muted),
                line,
            );
            draw_text(
                hdc,
                challenge,
                band(0.81, 0.89, 0.10),
                &mono(p.muted),
                wrapped,
            );
            let color = if challenge.starts_with(typed) {
                p.good
            } else {
                p.bad
            };
            draw_text(hdc, typed, band(0.90, 0.98, 0.10), &mono(color), wrapped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(challenge: Option<&str>) -> UiState {
        UiState {
            mode: Mode::Lock(LockView {
                until: NaiveDateTime::default(),
                reasons: String::new(),
                media: Vec::new(),
                challenge: challenge.map(str::to_string),
                emergency_minutes: 5,
            }),
            ..UiState::default()
        }
    }

    fn type_text(ui: &mut UiState, text: &str) {
        for c in text.chars() {
            ui.type_char(c as u32);
        }
    }

    #[test]
    fn typing_is_lowercased_and_backspace_works() {
        let mut ui = locked(Some("ab c2"));
        type_text(&mut ui, "Ax");
        assert_eq!(ui.typed, "ax");
        ui.type_char(8);
        type_text(&mut ui, "b c2");
        assert_eq!(ui.typed, "ab c2");
    }

    #[test]
    fn typing_never_exceeds_the_challenge_and_ignores_control_keys() {
        let mut ui = locked(Some("abc"));
        type_text(&mut ui, "zzzzzz\r\u{1b}");
        assert_eq!(ui.typed, "zzz");
    }

    #[test]
    fn typing_does_nothing_without_a_challenge() {
        let mut ui = locked(None);
        type_text(&mut ui, "abc");
        assert!(ui.typed.is_empty());
        let mut idle = UiState::default();
        type_text(&mut idle, "abc");
        assert!(idle.typed.is_empty());
    }

    #[test]
    fn countdown_switches_units_at_one_hour() {
        assert_eq!(countdown(chrono::Duration::seconds(299)), "4:59");
        assert_eq!(countdown(chrono::Duration::seconds(3900)), "1 h 05 min");
        assert_eq!(countdown(chrono::Duration::seconds(-5)), "0:00");
    }

    #[test]
    fn modes_survive_the_trip_to_the_agent() {
        let lock = locked(Some("abc")).mode;
        for mode in [Mode::Idle, Mode::Banner { text: "hi".into() }, lock] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(serde_json::from_str::<Mode>(&json).unwrap(), mode);
        }
    }
}
