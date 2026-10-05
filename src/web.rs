//! What the two web views share: the settings window and the lock screen
//! both show pages through WebView2 and both read photos and videos from the
//! media directory through the `media` protocol.

use std::borrow::Cow;
use std::num::NonZeroIsize;
use std::sync::{Arc, Mutex};

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::HWND;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::http::{Request, Response, StatusCode, header};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

use crate::i18n::Lang;
use crate::overlay::{LockView, UiState};
use crate::paths::Paths;
use crate::{log, media};

/// The page backgrounds, for the native window behind each web view: the
/// settings window's, and the dark screen the lock page puts its sticker on.
pub const LIGHT_BG: (u8, u8, u8, u8) = (241, 241, 246, 255);
pub const DARK_BG: (u8, u8, u8, u8) = (20, 19, 48, 255);
pub const LOCK_LIGHT_BG: (u8, u8, u8, u8) = (29, 27, 69, 255);
pub const LOCK_DARK_BG: (u8, u8, u8, u8) = (11, 10, 31, 255);

/// Paints a window's title bar and border in the colours of the navigation
/// pane under them, so the frame and the page read as one piece. Windows 11
/// does it; older versions ignore the request.
pub fn dress_frame(window: &Parent, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DwmSetWindowAttribute,
    };
    const fn rgb(r: u8, g: u8, b: u8) -> u32 {
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
    }
    let (caption, text, border) = if dark {
        (rgb(15, 14, 40), rgb(238, 237, 251), rgb(44, 42, 92))
    } else {
        (rgb(231, 231, 240), rgb(29, 27, 69), rgb(201, 201, 220))
    };
    let set = |attribute: i32, value: u32| unsafe {
        DwmSetWindowAttribute(
            window.0,
            attribute as u32,
            (&raw const value).cast(),
            size_of::<u32>() as u32,
        );
    };
    // Dark mode is what turns the minimise, maximise and close glyphs light.
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as u32);
    set(DWMWA_CAPTION_COLOR, caption);
    set(DWMWA_TEXT_COLOR, text);
    set(DWMWA_BORDER_COLOR, border);
}

/// A window of ours, for wry to put a web view inside.
pub struct Parent(pub HWND);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
        let handle = Win32WindowHandle::new(hwnd);
        // The handle is valid for as long as this Parent is, which is what the
        // borrow expresses.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

/// WebView2 keeps its cache next to the executable unless told otherwise, and
/// Program Files is read-only. Each web view gets its own directory because
/// two processes cannot share one.
pub fn context(paths: &Paths, name: &str) -> WebContext {
    WebContext::new(Some(paths.agent_log().with_file_name(name)))
}

/// Keeps the pages from behaving like a browser: no context menu, no
/// reloading or developer shortcuts, no dragging images around.
pub const PAGE_SCRIPT: &str = r#"
document.addEventListener("contextmenu", (e) => e.preventDefault());
document.addEventListener("dragstart", (e) => e.preventDefault());
document.addEventListener("keydown", (e) => {
  if (e.key === "F5" || e.key === "F12" || e.key === "F11" || (e.ctrlKey && "rpufsRPUFS".includes(e.key))) e.preventDefault();
});
"#;

/// Answers a `media` request with the file it names.
pub fn serve_media(paths: &Paths, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let name = request.uri().path().trim_start_matches('/');
    let name = percent_decode(name);
    let not_found = || {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&[][..]))
            .unwrap()
    };
    let Some(path) = media::file(paths, &name) else {
        return not_found();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return not_found();
    };
    let kind = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    };

    // Videos are fetched in ranges; without honouring them playback stalls.
    let total = bytes.len();
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| {
            let (start, end) = v.split_once('-')?;
            let start: usize = start.parse().ok()?;
            let end: usize = end.parse().unwrap_or(total.saturating_sub(1));
            (start < total).then_some((start, end.min(total - 1)))
        });
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    match range {
        Some((start, end)) => builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            )
            .header(header::CONTENT_LENGTH, end - start + 1)
            .body(Cow::Owned(bytes[start..=end].to_vec()))
            .unwrap(),
        None => builder
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, total)
            .body(Cow::Owned(bytes))
            .unwrap(),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(value);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Hands a web address to the default browser.
pub fn open_in_browser(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = crate::wide("open");
    let target = crate::wide(url);
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// The bundled typeface as a stylesheet, so the pages look the same offline.
pub fn font_css() -> String {
    use base64::Engine;
    const GABARITO: &[u8] = include_bytes!("../ui/fonts/gabarito-latin.woff2");
    let data = base64::engine::general_purpose::STANDARD.encode(GABARITO);
    format!(
        "@font-face {{ font-family: Gabarito; font-style: normal; font-weight: 400 900; font-display: block; src: url(data:font/woff2;base64,{data}) format(\"woff2\"); }}"
    )
}

/// Whether Windows is set to light or dark apps, for an appearance of "system".
pub fn windows_is_light() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|value| value != 0)
        .unwrap_or(true)
}

/// The `data-theme` attribute for the page: an explicit choice, or nothing
/// so that the page follows the system.
pub fn theme_attribute(appearance: &str) -> String {
    match appearance {
        "light" | "dark" => format!(" data-theme=\"{appearance}\""),
        _ => String::new(),
    }
}

/// Resolves "system" to what Windows is set to.
pub fn is_dark(appearance: &str) -> bool {
    match appearance {
        "dark" => true,
        "light" => false,
        _ => !windows_is_light(),
    }
}

/// The lock screen page with the texts and the view filled in.
pub fn lock_page(view: &LockView, lang: Lang, theme: &str) -> String {
    const HTML: &str = include_str!("../ui/lock.html");
    let mut texts = serde_json::Map::new();
    for (key, text) in lang.section("lock.") {
        texts.insert(key, Value::String(text));
    }
    let media: Vec<Value> = view
        .media
        .iter()
        .filter_map(|name| {
            let kind = match media::kind(name)? {
                media::Kind::Image => "image",
                media::Kind::Video => "video",
            };
            Some(json!({ "url": media::url(name), "kind": kind }))
        })
        .collect();
    let view = json!({
        "until": view.until.format("%Y-%m-%dT%H:%M:%S").to_string(),
        "reasons": view.reasons,
        "media": media,
        "challenge": view.challenge,
        "emergency_minutes": view.emergency_minutes,
    });
    HTML.replace("__I18N__", &Value::Object(texts).to_string())
        .replace("__VIEW__", &view.to_string())
        .replace("__LANG__", lang.code())
        .replace("__THEME__", &theme_attribute(theme))
        .replace("__FONT__", &font_css())
}

/// Puts the lock screen page inside `parent`, covering it. What the user
/// types towards the emergency challenge lands in `ui`.
pub fn build_lock(
    parent: HWND,
    (width, height): (i32, i32),
    view: &LockView,
    lang: Lang,
    theme: &str,
    paths: &Paths,
    ui: Arc<Mutex<UiState>>,
) -> wry::Result<WebView> {
    let mut context = context(paths, "webview-lock");
    let media_paths = paths.clone();
    let parent = Parent(parent);
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(lock_page(view, lang, theme))
        .with_bounds(Rect {
            position: PhysicalPosition::new(0, 0).into(),
            size: PhysicalSize::new(width as u32, height as u32).into(),
        })
        .with_background_color(if is_dark(theme) {
            LOCK_DARK_BG
        } else {
            LOCK_LIGHT_BG
        })
        .with_autoplay(true)
        .with_initialization_script(PAGE_SCRIPT)
        .with_custom_protocol("media".into(), move |_, request| {
            serve_media(&media_paths, &request)
        })
        .with_ipc_handler(move |request| {
            if let Ok(Value::Object(message)) = serde_json::from_str::<Value>(request.body())
                && let Some(Value::String(typed)) = message.get("typed")
            {
                ui.lock().unwrap().typed = typed.clone();
            }
        })
        .build_as_child(&parent);
    if let Err(e) = &webview {
        log!("lock page could not be shown, drawing it instead: {e}");
    }
    webview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_page_carries_the_view() {
        let view = LockView {
            until: chrono::NaiveDate::from_ymd_opt(2026, 10, 2)
                .unwrap()
                .and_hms_opt(23, 0, 0)
                .unwrap(),
            reasons: "Sleep.".into(),
            media: vec!["family.png".into(), "clip.mp4".into(), "notes.txt".into()],
            challenge: Some("abcde".into()),
            emergency_minutes: 5,
            idea: None,
        };
        let html = lock_page(&view, Lang::EN, "dark");
        assert!(html.contains("2026-10-02T23:00:00"));
        assert!(html.contains("http://media.localhost/family.png"));
        assert!(html.contains(r#""kind":"video""#));
        assert!(!html.contains("notes.txt"));
        assert!(html.contains("data-theme=\"dark\""));
        assert!(html.contains("font-family: Gabarito"));
        for placeholder in ["__I18N__", "__VIEW__", "__LANG__", "__THEME__", "__FONT__"] {
            assert!(!html.contains(placeholder));
        }
    }

    #[test]
    fn percent_encoding_is_undone() {
        assert_eq!(percent_decode("la%20familia.png"), "la familia.png");
        assert_eq!(percent_decode("plain.png"), "plain.png");
        assert_eq!(percent_decode("bad%2"), "bad%2");
    }
}
