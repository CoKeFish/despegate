//! Photos and videos that go with the reasons. They are copied into the data
//! directory so the lock screen can always find them.

use std::io;
use std::path::{Path, PathBuf};

use crate::paths::Paths;

const IMAGES: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];
const VIDEOS: &[&str] = &["mp4", "webm"];
/// Larger files make the lock screen slow to appear.
pub const MAX_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Image,
    Video,
}

#[derive(Debug)]
pub enum ImportError {
    Unsupported,
    TooLarge,
    Unreadable(io::Error),
    Unwritable(io::Error),
}

pub fn kind(name: &str) -> Option<Kind> {
    let extension = Path::new(name).extension()?.to_str()?.to_lowercase();
    if IMAGES.contains(&extension.as_str()) {
        Some(Kind::Image)
    } else if VIDEOS.contains(&extension.as_str()) {
        Some(Kind::Video)
    } else {
        None
    }
}

/// Where the page finds a file. The custom protocol turns this back into a
/// read from the media directory.
pub fn url(name: &str) -> String {
    format!("http://media.localhost/{name}")
}

/// Reads `source` in full, refusing files that are not media or too big.
/// Called by the daemon while it impersonates the user who asked, so it can
/// read exactly what that user can read.
pub fn read_source(source: &Path) -> Result<Vec<u8>, ImportError> {
    let name = source.file_name().and_then(|n| n.to_str()).unwrap_or("");
    kind(name).ok_or(ImportError::Unsupported)?;
    let size = std::fs::metadata(source)
        .map_err(ImportError::Unreadable)?
        .len();
    if size > MAX_BYTES {
        return Err(ImportError::TooLarge);
    }
    std::fs::read(source).map_err(ImportError::Unreadable)
}

/// Stores `bytes` under a safe name derived from `source` and returns that name.
pub fn store(paths: &Paths, source: &Path, bytes: &[u8]) -> Result<String, ImportError> {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("media");
    let extension = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut stem: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while stem.contains("--") {
        stem = stem.replace("--", "-");
    }
    let stem: String = stem.trim_matches('-').chars().take(40).collect();
    let stem = if stem.is_empty() {
        "media".to_string()
    } else {
        stem
    };

    let dir = paths.media_dir();
    std::fs::create_dir_all(&dir).map_err(ImportError::Unwritable)?;
    let mut name = format!("{stem}.{extension}");
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{stem}-{n}.{extension}");
        n += 1;
    }
    std::fs::write(dir.join(&name), bytes).map_err(ImportError::Unwritable)?;
    Ok(name)
}

pub fn remove(paths: &Paths, name: &str) {
    if let Some(path) = file(paths, name) {
        let _ = std::fs::remove_file(path);
    }
}

/// The file behind a stored name, if the name is one we could have produced.
pub fn file(paths: &Paths, name: &str) -> Option<PathBuf> {
    let safe = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !name.contains("..");
    (safe && kind(name).is_some()).then(|| paths.media_dir().join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_the_extension() {
        assert_eq!(kind("Foto.JPG"), Some(Kind::Image));
        assert_eq!(kind("clip.webm"), Some(Kind::Video));
        assert_eq!(kind("notes.txt"), None);
        assert_eq!(kind("noext"), None);
    }

    #[test]
    fn stored_names_are_safe_and_unique() {
        let dir = std::env::temp_dir().join(format!("despegate-media-{}", std::process::id()));
        let paths = Paths::new(Some(dir.clone()));
        let source = Path::new(r"C:\Users\Ana\Fotos\La familia (2026)!.PNG");
        let first = store(&paths, source, b"one").unwrap();
        let second = store(&paths, source, b"two").unwrap();
        assert_eq!(first, "la-familia-2026.png");
        assert_eq!(second, "la-familia-2026-2.png");
        assert_eq!(
            std::fs::read(file(&paths, &second).unwrap()).unwrap(),
            b"two"
        );
        remove(&paths, &first);
        assert!(!dir.join("media").join(&first).exists());
        assert_eq!(file(&paths, "../config.toml"), None);
        assert_eq!(file(&paths, "x.txt"), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
