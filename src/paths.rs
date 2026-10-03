use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Where the daemon keeps its state and how the CLI reaches it.
///
/// The installed system always uses `%ProgramData%\despegate`. A `--home`
/// override exists only for development: it is a command-line flag (never an
/// environment variable) so it cannot be used to point the installed daemon at
/// an empty config.
#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    /// Suffix that keeps a dev instance's pipe and mutex apart from the installed one.
    suffix: String,
}

impl Paths {
    pub fn new(home_override: Option<PathBuf>) -> Paths {
        match home_override {
            None => {
                let base = std::env::var_os("ProgramData")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
                Paths {
                    home: base.join("despegate"),
                    suffix: String::new(),
                }
            }
            Some(home) => {
                let home = std::path::absolute(&home).unwrap_or(home);
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                home.to_string_lossy().to_lowercase().hash(&mut hasher);
                Paths {
                    home,
                    suffix: format!("-dev-{:016x}", hasher.finish()),
                }
            }
        }
    }

    pub fn is_dev(&self) -> bool {
        !self.suffix.is_empty()
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    /// Usage counters that must survive a restart of the daemon.
    pub fn state(&self) -> PathBuf {
        self.home.join("state.toml")
    }

    /// Photos and videos that go with the reasons.
    pub fn media_dir(&self) -> PathBuf {
        self.home.join("media")
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("despegate.log")
    }

    /// The agent runs as the user, who cannot write to the installed home.
    pub fn agent_log(&self) -> PathBuf {
        if self.is_dev() {
            return self.home.join("agent.log");
        }
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("despegate")
            .join("agent.log")
    }

    /// While this file exists the daemon exits instead of running. The home
    /// directory is writable only by administrators once installed.
    pub fn stop_marker(&self) -> PathBuf {
        self.home.join("stop")
    }

    pub fn pipe(&self) -> String {
        format!(r"\\.\pipe\despegate{}", self.suffix)
    }

    /// Held by the one running daemon. The installed daemon lives in the
    /// services session, so its mutex must be visible from every session.
    pub fn mutex(&self) -> String {
        let namespace = if self.is_dev() { "Local" } else { "Global" };
        format!(r"{namespace}\despegate-daemon{}", self.suffix)
    }

    /// Arguments that make a child process use these same paths.
    pub fn args(&self) -> Vec<String> {
        if self.is_dev() {
            vec!["--home".into(), self.home.to_string_lossy().into_owned()]
        } else {
            Vec::new()
        }
    }
}
