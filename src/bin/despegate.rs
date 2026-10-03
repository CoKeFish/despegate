use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use chrono::{Local, NaiveTime, Weekday};
use clap::{Arg, ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};

use despegate::config::{Config, Rule, WEEK};
use despegate::i18n::Lang;
use despegate::paths::Paths;
use despegate::service::{self, Appearance, Request, Response, Runtime, Setting};
use despegate::store::Store;
use despegate::usage::State;
use despegate::{install, ipc, media, tr, ui};

// The help texts below are the English fallback; `localize` replaces them
// with the catalog's (`cli.*` in locales/) for the language in use.

/// Forces you to step away from the computer
#[derive(Parser)]
#[command(name = "despegate", version)]
struct Cli {
    /// Use a private data directory instead of the installed one (development)
    #[arg(long, global = true, hide = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show what is active, what comes next, and the configuration
    Status,
    /// Manage recurring blocks
    #[command(subcommand)]
    Rule(RuleCommand),
    /// Start a block right now; it cannot be cancelled
    Now {
        /// How long, e.g. 45m, 2h, 1h30m
        #[arg(long = "for", value_name = "DURATION", value_parser = parse_duration)]
        minutes: u32,
        /// Take over the whole screen
        #[arg(long)]
        lock: bool,
        /// Program to close (repeatable or comma-separated), e.g. steam.exe
        #[arg(long = "app", value_name = "EXE", value_delimiter = ',')]
        apps: Vec<String>,
    },
    /// Forced breaks after a stretch of continuous use
    #[command(subcommand)]
    Break(BreakCommand),
    /// Daily time budgets for individual programs
    #[command(subcommand)]
    Allowance(AllowanceCommand),
    /// The message with your reasons for installing despegate
    #[command(subcommand)]
    Reasons(ReasonsCommand),
    /// Light, dark, or follow Windows
    Appearance { mode: Appearance },
    /// Show or choose the language
    Language {
        /// A language code such as en or es, or auto to follow Windows
        code: Option<String>,
    },
    /// Change a setting
    Set { key: Setting, value: u32 },
    /// Install despegate so it starts with Windows and resists being stopped
    Install {
        #[arg(long, hide = true)]
        pause: bool,
    },
    /// Open the settings window
    Ui,
    /// Remove despegate, after reading your reasons
    Uninstall {
        #[arg(long, hide = true)]
        pause: bool,
    },
}

#[derive(Subcommand)]
enum RuleCommand {
    /// Add a recurring block
    Add {
        name: String,
        /// Start time, HH:MM
        #[arg(long, value_parser = parse_time)]
        from: NaiveTime,
        /// End time, HH:MM (earlier than --from means it ends the next day)
        #[arg(long, value_parser = parse_time)]
        to: NaiveTime,
        /// Days it starts on: all, weekdays, weekends, mon-fri, or mon,wed,sat
        #[arg(long, default_value = "all")]
        days: Days,
        /// Take over the whole screen
        #[arg(long)]
        lock: bool,
        /// Program to close (repeatable or comma-separated), e.g. steam.exe
        #[arg(long = "app", value_name = "EXE", value_delimiter = ',')]
        apps: Vec<String>,
    },
    /// List the rules
    List,
    /// Remove a rule (refused while it is active or about to start)
    Remove { name: String },
}

#[derive(Subcommand)]
enum BreakCommand {
    /// Lock the screen for a while after every stretch of use
    Set {
        /// Use allowed between breaks, e.g. 50m
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        every: u32,
        /// Length of the break, e.g. 10m
        #[arg(long = "for", value_name = "DURATION", value_parser = parse_duration)]
        rest: u32,
    },
    /// Turn forced breaks off
    Off,
}

#[derive(Subcommand)]
enum AllowanceCommand {
    /// Limit how long a program may be used per day
    Set {
        /// Executable name, e.g. hearthstone.exe
        app: String,
        /// Time per day, e.g. 60m or 1h30m
        #[arg(value_name = "DURATION", value_parser = parse_duration)]
        minutes: u32,
    },
    /// Remove a program's daily limit
    Remove {
        /// Executable name
        app: String,
    },
}

#[derive(Subcommand)]
enum ReasonsCommand {
    /// Write your reasons; with no text, reads them from standard input
    Set { text: Vec<String> },
    /// Add a photo or video (jpg, png, gif, webp, mp4, webm)
    Add {
        /// Path of the file to copy in
        file: PathBuf,
    },
    /// Remove a photo or video by its stored name
    Remove {
        /// Name as shown by `reasons show`
        name: String,
    },
    /// Print your reasons and the photos and videos that go with them
    Show,
}

#[derive(Clone)]
struct Days(Vec<Weekday>);

impl FromStr for Days {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let day = |s: &str| {
            s.trim()
                .parse::<Weekday>()
                .map_err(|_| format!("'{s}' is not a day"))
        };
        let mut days = Vec::new();
        for part in text.split(',') {
            match part.trim().to_lowercase().as_str() {
                "all" | "daily" | "everyday" => days.extend(WEEK),
                "weekdays" => days.extend(&WEEK[..5]),
                "weekends" => days.extend(&WEEK[5..]),
                range if range.contains('-') => {
                    let (first, last) = range.split_once('-').unwrap();
                    let mut current = day(first)?;
                    let last = day(last)?;
                    days.push(current);
                    while current != last {
                        current = current.succ();
                        days.push(current);
                    }
                }
                single => days.push(day(single)?),
            }
        }
        Ok(Days(
            WEEK.into_iter().filter(|d| days.contains(d)).collect(),
        ))
    }
}

fn parse_time(text: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(text, "%H:%M")
        .map_err(|_| format!("'{text}' is not a time like 23:00"))
}

/// Minutes from "45m", "2h", "1h30m" or a bare number of minutes.
fn parse_duration(text: &str) -> Result<u32, String> {
    let invalid = || format!("'{text}' is not a duration like 45m, 2h or 1h30m");
    let mut total: u32 = 0;
    let mut digits = String::new();
    for c in text.trim().chars() {
        match c {
            '0'..='9' => digits.push(c),
            'h' | 'm' => {
                let n: u32 = digits.parse().map_err(|_| invalid())?;
                total = total.saturating_add(if c == 'h' { n.saturating_mul(60) } else { n });
                digits.clear();
            }
            _ => return Err(invalid()),
        }
    }
    if !digits.is_empty() {
        total = total.saturating_add(digits.parse().map_err(|_| invalid())?);
    }
    if total == 0 {
        Err(invalid())
    } else {
        Ok(total)
    }
}

/// Replaces the built-in English help of `cmd` and everything under it with
/// the catalog's. `path` is the dotted chain of subcommand names below `cli`.
fn localize(mut cmd: clap::Command, path: &str, lang: Lang) -> clap::Command {
    let text = |key: String| lang.has(&key).then(|| lang.format(&key, &[]));
    if let Some(about) = text(format!("{path}.about")) {
        cmd = match text(format!("{path}.long_about")) {
            Some(long) => cmd.about(about).long_about(long),
            None => cmd.about(about).long_about(None::<&str>),
        };
    }
    if cmd.get_version().is_some() {
        cmd = cmd.disable_version_flag(true).arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help(tr!(lang, "cli.version_flag"))
                .help_heading(tr!(lang, "cli.heading.options")),
        );
    }
    let options = tr!(lang, "cli.heading.options");
    let arguments = tr!(lang, "cli.heading.arguments");

    let args: Vec<(String, bool)> = cmd
        .get_arguments()
        .map(|a| (a.get_id().to_string(), a.is_positional()))
        .collect();
    for (id, positional) in args {
        let heading = if positional {
            arguments.clone()
        } else {
            options.clone()
        };
        let help = text(format!("{path}.arg.{id}"));
        cmd = cmd.mut_arg(&id, |arg| {
            let arg = arg.help_heading(heading);
            // The catalog text already lists the values a setting can take.
            let takes_values = arg.get_action().takes_values();
            match help {
                Some(help) => arg
                    .help(help)
                    .long_help(None::<&str>)
                    .hide_possible_values(takes_values),
                None => arg,
            }
        });
    }
    // clap would add its own -h with an English description.
    cmd = cmd
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help(tr!(lang, "cli.help_flag"))
                .help_heading(options),
        );
    cmd = cmd
        .subcommand_help_heading(tr!(lang, "cli.heading.commands"))
        .subcommand_value_name(tr!(lang, "cli.heading.command"))
        .help_template(format!(
            "{{about-with-newline}}\n{} {{usage}}\n\n{{all-args}}",
            tr!(lang, "cli.heading.usage")
        ));

    let subcommands: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for name in subcommands {
        cmd = cmd.mut_subcommand(&name, |sub| localize(sub, &format!("{path}.{name}"), lang));
    }
    cmd
}

/// The `--home` value, needed before the arguments are parsed properly
/// because the language to parse them in is stored there.
fn home_argument() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == "--home")
        .and_then(|at| args.get(at + 1))
        .map(PathBuf::from)
}

fn main() -> ExitCode {
    let paths = Paths::new(home_argument());
    let config: Config = Store::peek(&paths.config());
    let lang = Lang::resolve(config.language.as_deref(), Some(Lang::system().code()));

    let matches = localize(Cli::command(), "cli", lang).get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let request = match cli.command {
        Command::Status => Request::Status,
        Command::Rule(RuleCommand::Add {
            name,
            from,
            to,
            days,
            lock,
            apps,
        }) => Request::RuleAdd {
            rule: Rule {
                name,
                days: days.0,
                start: from,
                end: to,
                lock,
                apps,
            },
        },
        Command::Rule(RuleCommand::List) => {
            if config.rules.is_empty() {
                println!("{}", tr!(lang, "rule.none"));
            }
            for rule in &config.rules {
                println!("{}", service::describe_rule(rule, lang));
            }
            return ExitCode::SUCCESS;
        }
        Command::Rule(RuleCommand::Remove { name }) => Request::RuleRemove { name },
        Command::Now {
            minutes,
            lock,
            apps,
        } => Request::Now {
            minutes,
            lock,
            apps,
        },
        Command::Break(BreakCommand::Set { every, rest }) => Request::BreakSet {
            work_minutes: every,
            break_minutes: rest,
        },
        Command::Break(BreakCommand::Off) => Request::BreakOff,
        Command::Allowance(AllowanceCommand::Set { app, minutes }) => {
            Request::AllowanceSet { app, minutes }
        }
        Command::Allowance(AllowanceCommand::Remove { app }) => Request::AllowanceRemove { app },
        Command::Reasons(ReasonsCommand::Set { text }) => {
            let text = if text.is_empty() {
                let mut input = String::new();
                if let Err(e) = std::io::stdin().read_to_string(&mut input) {
                    return fail(&e.to_string());
                }
                input
            } else {
                text.join(" ")
            };
            if text.trim().is_empty() {
                return fail(&tr!(lang, "error.reasons_empty"));
            }
            Request::ReasonsSet { text }
        }
        Command::Reasons(ReasonsCommand::Add { file }) => {
            let file = std::path::absolute(&file).unwrap_or(file);
            Request::MediaImport {
                source: file.to_string_lossy().into_owned(),
            }
        }
        Command::Reasons(ReasonsCommand::Remove { name }) => Request::MediaRemove { name },
        Command::Reasons(ReasonsCommand::Show) => {
            if config.reasons.is_empty() {
                println!("{}", tr!(lang, "reasons.none"));
            } else {
                println!("{}", config.reasons);
            }
            for name in &config.media {
                println!("  {name}");
            }
            return ExitCode::SUCCESS;
        }
        Command::Language { code: None } => {
            let current = match &config.language {
                Some(_) => tr!(lang, "status.language", name = lang.name()),
                None => tr!(lang, "status.language_auto", name = lang.name()),
            };
            println!("{current}");
            for available in Lang::all() {
                println!("  {}  {}", available.code(), available.name());
            }
            return ExitCode::SUCCESS;
        }
        Command::Language { code: Some(code) } => Request::LanguageSet {
            code: (code != "auto").then_some(code),
        },
        Command::Appearance { mode } => Request::AppearanceSet { mode },
        Command::Set { key, value } => Request::Set { key, value },
        Command::Ui => {
            let exe = std::env::current_exe().map(|exe| exe.with_file_name(ui::UI_EXE));
            let started =
                exe.and_then(|exe| std::process::Command::new(exe).args(paths.args()).spawn());
            return match started {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => fail(&tr!(lang, "error.no_ui", error = e)),
            };
        }
        Command::Install { pause } => return finish(install::install(&paths, lang), pause, lang),
        Command::Uninstall { pause } => {
            return finish(install::uninstall(&paths, lang), pause, lang);
        }
    };

    let response = send(&paths, request, lang);
    if response.ok {
        println!("{}", response.message);
        ExitCode::SUCCESS
    } else {
        fail(&response.message)
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("despegate: {message}");
    ExitCode::FAILURE
}

fn finish(result: Result<(), String>, pause: bool, lang: Lang) -> ExitCode {
    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(&message),
    };
    if pause {
        install::pause(lang);
    }
    code
}

fn refusal(message: String) -> Response {
    Response {
        ok: false,
        message,
        view: None,
    }
}

/// Asks the daemon; when none is running, applies the request to the config
/// file directly under the same rules.
fn send(paths: &Paths, request: Request, lang: Lang) -> Response {
    match ipc::request(&paths.pipe(), request.clone()) {
        Ok(Some(response)) => response,
        Ok(None) => apply_locally(paths, request, lang),
        Err(e) => refusal(tr!(lang, "error.daemon_unreachable", error = e)),
    }
}

fn apply_locally(paths: &Paths, request: Request, lang: Lang) -> Response {
    let now = Local::now().naive_local();
    let state: State = Store::peek(&paths.state());
    let runtime = Runtime {
        daemon: false,
        paused_until: None,
        state: &state,
    };
    if matches!(request, Request::Status) {
        let mut config: Config = Store::peek(&paths.config());
        return service::handle(request, &mut config, now, &runtime, Lang::system()).response;
    }
    let mut store: Store<Config> = match Store::open(&paths.config()) {
        Ok(store) => store,
        Err(e) => return refusal(tr!(lang, "error.no_daemon", error = e)),
    };
    let request = match request {
        Request::MediaImport { source } => {
            let source = PathBuf::from(source);
            match media::read_source(&source).and_then(|bytes| media::store(paths, &source, &bytes))
            {
                Ok(name) => Request::MediaAdd { name },
                Err(e) => return refusal(service::import_error(e, lang)),
            }
        }
        other => other,
    };
    let outcome = service::handle(
        request.clone(),
        &mut store.data,
        now,
        &runtime,
        Lang::system(),
    );
    if outcome.changed
        && let Request::MediaRemove { name } = &request
    {
        media::remove(paths, name);
    }
    if outcome.changed
        && let Err(e) = store.save()
    {
        return refusal(tr!(lang, "error.save", error = e));
    }
    outcome.response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command and visible argument must have a help text in the catalog.
    #[test]
    fn the_catalog_documents_the_whole_cli() {
        fn check(cmd: &clap::Command, path: &str, missing: &mut Vec<String>) {
            let mut need = |key: String| {
                if !Lang::EN.has(&key) {
                    missing.push(key);
                }
            };
            need(format!("{path}.about"));
            for arg in cmd.get_arguments().filter(|a| !a.is_hide_set()) {
                need(format!("{path}.arg.{}", arg.get_id()));
            }
            for sub in cmd.get_subcommands() {
                check(sub, &format!("{path}.{}", sub.get_name()), missing);
            }
        }
        let mut missing = Vec::new();
        check(&Cli::command(), "cli", &mut missing);
        assert!(missing.is_empty(), "missing from en.toml: {missing:#?}");
    }

    #[test]
    fn help_is_translated() {
        let es = Lang::from_code("es").unwrap();
        let mut cmd = localize(Cli::command(), "cli", es);
        let help = cmd.render_help().to_string();
        assert!(
            help.contains("Uso:") && help.contains("Comandos:"),
            "{help}"
        );
        // The localized tree must still be a valid clap command.
        localize(Cli::command(), "cli", es).debug_assert();
        Cli::command().debug_assert();
    }

    #[test]
    fn durations_and_days_are_parsed() {
        assert_eq!(parse_duration("45m"), Ok(45));
        assert_eq!(parse_duration("1h30m"), Ok(90));
        assert_eq!(parse_duration("90"), Ok(90));
        assert!(parse_duration("soon").is_err() && parse_duration("0m").is_err());
        assert_eq!("weekdays".parse::<Days>().unwrap().0, WEEK[..5]);
        assert_eq!(
            "sat-mon".parse::<Days>().unwrap().0,
            [Weekday::Mon, Weekday::Sat, Weekday::Sun]
        );
        assert!("someday".parse::<Days>().is_err());
    }
}
