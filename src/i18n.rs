//! Every text shown to the user comes from a catalog in `locales/`.
//!
//! To add a language, write `locales/<code>.toml` with the same keys as
//! `en.toml` and list it in [`SOURCES`]; the tests below check that nothing
//! is missing. English is the fallback for a key a catalog lacks.

use std::collections::HashMap;
use std::sync::OnceLock;

use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

/// Code, name in the language itself, and catalog. English must stay first.
const SOURCES: &[(&str, &str, &str)] = &[
    ("en", "English", include_str!("../locales/en.toml")),
    ("es", "Español", include_str!("../locales/es.toml")),
];

type Catalog = HashMap<String, String>;

fn catalogs() -> &'static [Catalog] {
    static CATALOGS: OnceLock<Vec<Catalog>> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(code, _, text)| parse(code, text))
            .collect()
    })
}

/// Flattens the TOML tables into dotted keys: `[rule] added` is `rule.added`.
fn parse(code: &str, text: &str) -> Catalog {
    fn flatten(prefix: &str, table: &toml::Table, out: &mut Catalog) {
        for (key, value) in table {
            let key = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match value {
                toml::Value::Table(inner) => flatten(&key, inner, out),
                toml::Value::String(text) => {
                    out.insert(key, text.clone());
                }
                other => panic!("locale {key} must be text, found {other}"),
            }
        }
    }
    let table: toml::Table =
        toml::from_str(text).unwrap_or_else(|e| panic!("locales/{code}.toml is not valid: {e}"));
    let mut catalog = Catalog::new();
    flatten("", &table, &mut catalog);
    catalog
}

/// One of the available languages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lang(usize);

impl Default for Lang {
    fn default() -> Self {
        Lang::EN
    }
}

impl Lang {
    pub const EN: Lang = Lang(0);

    pub fn all() -> impl Iterator<Item = Lang> {
        (0..SOURCES.len()).map(Lang)
    }

    /// Accepts a bare code or a full locale name: `es`, `es-CO`, `es_ES`.
    pub fn from_code(code: &str) -> Option<Lang> {
        let primary = code.split(['-', '_']).next()?.trim().to_lowercase();
        SOURCES
            .iter()
            .position(|(code, _, _)| *code == primary)
            .map(Lang)
    }

    /// The language Windows is displayed in for the calling user.
    pub fn system() -> Lang {
        let mut name = [0u16; 85];
        let len = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
        let len = (len.max(1) - 1) as usize;
        Lang::from_code(&String::from_utf16_lossy(&name[..len])).unwrap_or(Lang::EN)
    }

    /// The configured language, or else the hinted one (the language of
    /// whoever is asking), or else English.
    pub fn resolve(setting: Option<&str>, hint: Option<&str>) -> Lang {
        setting
            .and_then(Lang::from_code)
            .or_else(|| hint.and_then(Lang::from_code))
            .unwrap_or(Lang::EN)
    }

    pub fn code(self) -> &'static str {
        SOURCES[self.0].0
    }

    pub fn name(self) -> &'static str {
        SOURCES[self.0].1
    }

    fn template(self, key: &str) -> Option<&'static str> {
        let all = catalogs();
        all[self.0]
            .get(key)
            .or_else(|| all[Lang::EN.0].get(key))
            .map(String::as_str)
    }

    pub fn has(self, key: &str) -> bool {
        self.template(key).is_some()
    }

    /// The text for `key` with each `{name}` replaced by its argument. A key
    /// nobody translated shows up as the key itself, which is easy to spot.
    pub fn format(self, key: &str, args: &[(&str, String)]) -> String {
        let Some(template) = self.template(key) else {
            return key.to_string();
        };
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                rest = &rest[open..];
                break;
            };
            match args.iter().find(|(name, _)| *name == &after[..close]) {
                Some((_, value)) => out.push_str(value),
                None => out.push_str(&rest[open..open + close + 2]),
            }
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// `tr!(lang, "rule.removed", name = rule.name)`
#[macro_export]
macro_rules! tr {
    ($lang:expr, $key:literal) => {
        $lang.format($key, &[])
    };
    ($lang:expr, $key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $lang.format($key, &[$((stringify!($name), $value.to_string())),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else { break };
            found.insert(after[..close].to_string());
            rest = &after[close + 1..];
        }
        found
    }

    #[test]
    fn placeholders_are_replaced() {
        assert_eq!(
            tr!(Lang::EN, "rule.removed", name = "sleep"),
            "Removed rule 'sleep'"
        );
        assert_eq!(Lang::EN.format("no.such.key", &[]), "no.such.key");
    }

    #[test]
    fn codes_and_locale_names_are_recognised() {
        let es = Lang::from_code("es-CO").unwrap();
        assert_eq!(es.code(), "es");
        assert_eq!(Lang::from_code("ES_es"), Some(es));
        assert_eq!(Lang::from_code("xx"), None);
        assert_eq!(Lang::resolve(None, Some("es-MX")), es);
        assert_eq!(Lang::resolve(Some("en"), Some("es")), Lang::EN);
        assert_eq!(Lang::resolve(Some("xx"), None), Lang::EN);
    }

    #[test]
    fn every_language_translates_every_key_with_the_same_placeholders() {
        let all = catalogs();
        let english = &all[Lang::EN.0];
        for lang in Lang::all().skip(1) {
            let catalog = &all[lang.0];
            for (key, text) in english {
                let translated = catalog
                    .get(key)
                    .unwrap_or_else(|| panic!("{}.toml lacks '{key}'", lang.code()));
                assert_eq!(
                    placeholders(translated),
                    placeholders(text),
                    "{}.toml: placeholders of '{key}' differ from English",
                    lang.code()
                );
            }
            for key in catalog.keys() {
                assert!(
                    english.contains_key(key),
                    "{}.toml has unknown key '{key}'",
                    lang.code()
                );
            }
        }
    }

    /// Catches a `tr!` call whose key is misspelt or was never written.
    #[test]
    fn every_key_used_in_the_code_exists() {
        fn scan(dir: &std::path::Path, missing: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path, missing);
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let mut from = 0;
                while let Some(at) = source[from..].find("tr!(") {
                    let start = from + at;
                    from = start + 4;
                    // Part of a longer name (include_str!, say) is not a call.
                    let named = |c: char| c.is_alphanumeric() || c == '_';
                    let in_longer_name = source[..start].chars().next_back().is_some_and(named);
                    // The key is the string literal right after the language argument.
                    let rest = &source[from..];
                    let Some(open) = rest.find('"') else { break };
                    let language = rest[..open].trim_end();
                    if in_longer_name || !language.ends_with(',') || language.contains([')', '$']) {
                        continue;
                    }
                    let Some(len) = rest[open + 1..].find('"') else {
                        break;
                    };
                    let key = &rest[open + 1..open + 1 + len];
                    if !Lang::EN.has(key) {
                        missing.push(format!("{}: {key}", path.display()));
                    }
                }
            }
        }
        let mut missing = Vec::new();
        scan(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut missing,
        );
        assert!(
            missing.is_empty(),
            "keys missing from en.toml: {missing:#?}"
        );
    }
}
