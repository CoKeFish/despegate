//! The settings window: a native window showing an embedded page. Every
//! change the page makes goes through the CLI.
#![windows_subsystem = "windows"]

use std::path::PathBuf;

use despegate::paths::Paths;
use despegate::{tr, ui, web};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::IconExtWindows;
use tao::window::{Icon, WindowBuilder};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
    OPENFILENAMEW,
};
use wry::DragDropEvent;
use wry::WebViewBuilder;

/// Something for the page, carried from wherever it happened to the event
/// loop, which is the only place the web view can be scripted from.
enum Reply {
    /// An answer to a request, as JSON.
    Answer(String),
    /// Files dropped on the window.
    Dropped(Vec<PathBuf>),
    Dragging(bool),
}

/// The native "open file" dialog, for photos and videos. Returns the chosen
/// paths; several can be chosen at once.
fn pick_files(
    owner: windows_sys::Win32::Foundation::HWND,
    lang: despegate::i18n::Lang,
) -> Vec<PathBuf> {
    let title = despegate::wide(&tr!(lang, "ui.reasons.pick_title"));
    let filter: Vec<u16> = tr!(lang, "ui.reasons.pick_filter")
        .encode_utf16()
        .chain("\0*.jpg;*.jpeg;*.png;*.gif;*.webp;*.mp4;*.webm\0\0".encode_utf16())
        .collect();
    let mut buffer = vec![0u16; 32 * 1024];
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = owner;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrFile = buffer.as_mut_ptr();
    dialog.nMaxFile = buffer.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.Flags = OFN_EXPLORER | OFN_ALLOWMULTISELECT | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        return Vec::new();
    }
    // One path, or a directory followed by file names, each NUL-terminated.
    let parts: Vec<String> = buffer
        .split(|c| *c == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    match parts.as_slice() {
        [] => Vec::new(),
        [single] => vec![PathBuf::from(single)],
        [dir, files @ ..] => files.iter().map(|f| PathBuf::from(dir).join(f)).collect(),
    }
}

fn main() {
    // Usage: despegate-ui [--home DIR]
    let mut args = std::env::args().skip(1);
    let mut home = None;
    while let Some(arg) = args.next() {
        if arg == "--home" {
            home = args.next().map(PathBuf::from);
        }
    }
    let paths = Paths::new(home);
    let lang = ui::language(&paths);
    let appearance =
        despegate::store::Store::<despegate::config::Config>::peek(&paths.config()).appearance;

    let event_loop = EventLoopBuilder::<Reply>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(tr!(lang, "ui.title"))
        // The icon build.rs embedded in this executable.
        .with_window_icon(Icon::from_resource(1, None).ok())
        .with_inner_size(LogicalSize::new(960.0, 740.0))
        .with_min_inner_size(LogicalSize::new(600.0, 460.0))
        .build(&event_loop)
        .expect("cannot create the window");

    let mut context = web::context(&paths, "webview");
    let hwnd = match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => {
            handle.hwnd.get() as windows_sys::Win32::Foundation::HWND
        }
        _ => std::ptr::null_mut(),
    };
    web::dress_frame(&web::Parent(hwnd), web::is_dark(&appearance));

    let proxy = event_loop.create_proxy();
    let drop_proxy = proxy.clone();
    let bridge_paths = paths.clone();
    let media_paths = paths.clone();
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(ui::page(&paths, lang))
        .with_background_color(if web::is_dark(&appearance) {
            web::DARK_BG
        } else {
            web::LIGHT_BG
        })
        .with_initialization_script(web::PAGE_SCRIPT)
        .with_custom_protocol("media".into(), move |_, request| {
            web::serve_media(&media_paths, &request)
        })
        // Links to the web open in the browser; the page itself never leaves.
        .with_navigation_handler(|url| {
            if url.starts_with("http://") || url.starts_with("https://") {
                if !url.starts_with("http://media.localhost") {
                    web::open_in_browser(&url);
                }
                return false;
            }
            true
        })
        .with_drag_drop_handler(move |event| {
            let _ = match event {
                DragDropEvent::Enter { .. } => drop_proxy.send_event(Reply::Dragging(true)),
                DragDropEvent::Leave => drop_proxy.send_event(Reply::Dragging(false)),
                DragDropEvent::Drop { paths, .. } => drop_proxy.send_event(Reply::Dropped(paths)),
                _ => Ok(()),
            };
            true
        })
        .with_ipc_handler(move |request| {
            let message = request.body().clone();
            let parsed: serde_json::Value = serde_json::from_str(&message).unwrap_or_default();
            match parsed.get("kind").and_then(|kind| kind.as_str()) {
                // The page says which theme it is showing, for the frame to match.
                Some("theme") => {
                    let dark = parsed.get("dark").and_then(|dark| dark.as_bool());
                    web::dress_frame(&web::Parent(hwnd), dark.unwrap_or(false));
                }
                // The file dialog must run on this thread; it pumps messages itself.
                Some("pick") => {
                    let id = parsed.get("id").cloned().unwrap_or_default();
                    let paths: Vec<String> = pick_files(hwnd, lang)
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    let reply = serde_json::json!({ "id": id, "paths": paths }).to_string();
                    let _ = proxy.send_event(Reply::Answer(reply));
                }
                // The CLI takes a moment; answering off the UI thread keeps the
                // window responsive.
                _ => {
                    let paths = bridge_paths.clone();
                    let proxy = proxy.clone();
                    std::thread::spawn(move || {
                        let _ = proxy.send_event(Reply::Answer(ui::handle(&paths, &message)));
                    });
                }
            }
        })
        .build(&window)
        .expect("cannot create the web view (is the WebView2 runtime installed?)");

    let mut webview = Some(webview);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let _ = webview.take();
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(reply) => {
                if let Some(webview) = &webview {
                    let script = match reply {
                        Reply::Answer(json) => format!("window.__reply({json})"),
                        Reply::Dropped(paths) => {
                            let paths: Vec<String> = paths
                                .iter()
                                .map(|p| p.to_string_lossy().into_owned())
                                .collect();
                            format!(
                                "window.__dragging(false); window.__dropped({})",
                                serde_json::json!(paths)
                            )
                        }
                        Reply::Dragging(over) => format!("window.__dragging({over})"),
                    };
                    let _ = webview.evaluate_script(&script);
                }
            }
            _ => {}
        }
    });
}
