//! The bridge behind the settings window: the page asks for the current
//! state, and every change it makes is an invocation of the CLI.

use std::os::windows::process::CommandExt;
use std::process::Command;

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;
use crate::i18n::Lang;
use crate::overlay::countdown;
use crate::paths::Paths;
use crate::service::{self, Request};
use crate::store::Store;
use crate::usage::State;
use crate::{ipc, media, tr};

pub const UI_EXE: &str = "despegate-ui.exe";
const CLI_EXE: &str = "despegate.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What the page sends: `{"id": 1, "kind": "state"}` or
/// `{"id": 2, "kind": "cli", "args": ["rule", "remove", "sleep"]}`.
#[derive(Deserialize)]
struct Message {
    id: u64,
    kind: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Serialize)]
struct CliReply {
    ok: bool,
    text: String,
}

/// Answers one message from the page with the JSON the page expects back.
pub fn handle(paths: &Paths, message: &str) -> String {
    let reply = match serde_json::from_str::<Message>(message) {
        Ok(message) => match message.kind.as_str() {
            "state" => json!({ "id": message.id, "state": state(paths) }),
            "cli" => {
                let CliReply { ok, text } = run_cli(paths, &message.args);
                json!({ "id": message.id, "ok": ok, "text": text })
            }
            other => {
                json!({ "id": message.id, "ok": false, "text": format!("unknown message {other}") })
            }
        },
        Err(e) => json!({ "ok": false, "text": e.to_string() }),
    };
    reply.to_string()
}

/// The language the page is shown in: the configured one, else Windows'.
pub fn language(paths: &Paths) -> Lang {
    let config: Config = Store::peek(&paths.config());
    Lang::resolve(config.language.as_deref(), Some(Lang::system().code()))
}

/// Everything the page renders, in one piece.
fn state(paths: &Paths) -> Value {
    let now = Local::now().naive_local();
    let config: Config = Store::peek(&paths.config());
    let state: State = Store::peek(&paths.state());
    let lang = language(paths);

    let status = ipc::request(&paths.pipe(), Request::Status).ok().flatten();
    let daemon = status.is_some();
    let paused = status
        .as_ref()
        .map(|r| r.message.contains(&tr!(lang, "status.paused", until = "")))
        .unwrap_or(false);

    let active: Vec<String> = config
        .active_blocks(now)
        .into_iter()
        .map(|b| b.name)
        .collect();
    let left: serde_json::Map<String, Value> = config
        .allowances
        .iter()
        .filter_map(|a| {
            let left = state.allowance_left(&config, &a.app, now)?;
            Some((a.app.clone(), Value::String(countdown(left))))
        })
        .collect();
    let media: Vec<Value> = config
        .media
        .iter()
        .filter_map(|name| {
            let kind = match media::kind(name)? {
                media::Kind::Image => "image",
                media::Kind::Video => "video",
            };
            Some(json!({ "name": name, "url": media::url(name), "kind": kind }))
        })
        .collect();
    let languages: Vec<Value> = Lang::all()
        .map(|l| json!({ "code": l.code(), "name": l.name() }))
        .collect();

    json!({
        "daemon": daemon,
        "lang": lang.code(),
        "languages": languages,
        "config": config,
        "active": active,
        "allowance_left": left,
        "media": media,
        "appearance": config.appearance,
        "headline": headline(&config, &state, daemon, paused, lang),
        "status": status.map(|r| r.message).unwrap_or_default(),
    })
}

/// A sentence on what despegate is doing right now, one or two on what comes
/// next, and the kind of moment it is, which the page dresses differently.
fn headline(config: &Config, state: &State, daemon: bool, paused: bool, lang: Lang) -> Value {
    let now = Local::now().naive_local();
    let mut details: Vec<String> = Vec::new();
    if !daemon {
        return json!({ "kind": "stopped", "title": tr!(lang, "ui.hero.stopped"), "details": [tr!(lang, "ui.hero.stopped_detail")] });
    }

    let blocks = config.active_blocks(now);
    let (kind, title) = if paused {
        ("paused", tr!(lang, "ui.hero.paused"))
    } else if let Some(until) = state.on_break(now) {
        (
            "break",
            tr!(lang, "ui.hero.on_break", until = until.format("%H:%M")),
        )
    } else if let Some(lock) = blocks.iter().filter(|b| b.lock).max_by_key(|b| b.until) {
        (
            "locked",
            tr!(
                lang,
                "ui.hero.locked",
                name = lock.name,
                until = service::when(lang, lock.until)
            ),
        )
    } else if !blocks.is_empty() {
        let apps: Vec<&str> = blocks
            .iter()
            .flat_map(|b| b.apps.iter().map(String::as_str))
            .collect();
        let until = blocks.iter().map(|b| b.until).max().unwrap();
        (
            "blocking",
            tr!(
                lang,
                "ui.hero.blocking",
                apps = apps.join(", "),
                until = service::when(lang, until)
            ),
        )
    } else {
        ("free", tr!(lang, "ui.hero.free"))
    };

    match config.next_start(now) {
        Some((rule, start)) => {
            details.push(tr!(
                lang,
                "ui.hero.next",
                name = rule.name,
                when = service::when(lang, start),
                left = countdown(start - now)
            ));
        }
        None if config.rules.is_empty()
            && config.breaks.is_none()
            && config.allowances.is_empty() =>
        {
            details.push(tr!(lang, "ui.hero.empty"));
        }
        None => {}
    }
    if config.breaks.is_some()
        && state.on_break(now).is_none()
        && let Some(left) = state.break_due_in(config)
    {
        details.push(tr!(lang, "ui.hero.break_in", left = countdown(left)));
    }
    json!({ "kind": kind, "title": title, "details": details })
}

/// Runs `despegate <args>` and hands back what it printed.
fn run_cli(paths: &Paths, args: &[String]) -> CliReply {
    let Ok(exe) = std::env::current_exe() else {
        return CliReply {
            ok: false,
            text: "cannot locate despegate.exe".into(),
        };
    };
    let output = Command::new(exe.with_file_name(CLI_EXE))
        .args(paths.args())
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim().trim_start_matches("despegate: ");
            if !stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(stderr);
            }
            CliReply {
                ok: output.status.success(),
                text,
            }
        }
        Err(e) => CliReply {
            ok: false,
            text: e.to_string(),
        },
    }
}

/// The page with the catalog texts and the language filled in.
pub fn page(paths: &Paths, lang: Lang) -> String {
    const HTML: &str = include_str!("../ui/index.html");
    let mut texts = serde_json::Map::new();
    for (key, text) in lang.section("ui.").into_iter().chain(lang.section("day.")) {
        texts.insert(key, Value::String(text));
    }
    let config: Config = Store::peek(&paths.config());
    HTML.replace("__I18N__", &Value::Object(texts).to_string())
        .replace(
            "__THEME__",
            &crate::web::theme_attribute(&config.appearance),
        )
        .replace("__FONT__", &crate::web::font_css())
        .replace("__LANG__", lang.code())
        .replace("__VERSION__", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_filled_in_for_every_language() {
        let paths = Paths::new(Some(std::env::temp_dir().join("despegate-ui-test")));
        for lang in Lang::all() {
            let html = page(&paths, lang);
            for placeholder in ["__I18N__", "__LANG__", "__VERSION__"] {
                assert!(
                    !html.contains(placeholder),
                    "{placeholder} left in the page"
                );
            }
            assert!(html.contains(&format!("<html lang=\"{}\">", lang.code())));
            assert!(html.contains("window.__reply"));
        }
    }

    /// A newline inside a JavaScript string literal leaves the window blank.
    #[test]
    fn no_string_literal_in_the_script_spans_lines() {
        let lock = crate::overlay::LockView {
            until: Default::default(),
            reasons: String::new(),
            media: Vec::new(),
            challenge: None,
            emergency_minutes: 5,
        };
        let paths = Paths::new(Some(std::env::temp_dir().join("despegate-ui-test")));
        for html in [
            page(&paths, Lang::EN),
            crate::web::lock_page(&lock, Lang::EN, "system"),
        ] {
            let script = html
                .split("<script>")
                .nth(1)
                .unwrap()
                .split("</script>")
                .next()
                .unwrap();
            for (number, line) in script.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let quotes = code.matches('"').count() - code.matches("\\\"").count();
                assert!(
                    quotes % 2 == 0,
                    "unbalanced quotes in script line {}: {line}",
                    number + 1
                );
            }
        }
    }
}
