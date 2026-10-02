use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

const FILE_SHARE_READ: u32 = 1;

/// A TOML file held open for as long as the store lives.
///
/// Others may read it but not write, rename or delete it, so while the daemon
/// runs the only way to change what it holds is to ask the daemon.
pub struct Store<T> {
    file: File,
    pub data: T,
}

impl<T: Serialize + DeserializeOwned + Default> Store<T> {
    pub fn open(path: &Path) -> io::Result<Store<T>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ)
            .open(path)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let data = toml::from_str(&text).unwrap_or_else(|e| {
            crate::log!(
                "{} is unreadable, starting from defaults: {e}",
                path.display()
            );
            T::default()
        });
        Ok(Store { file, data })
    }

    /// Reads the file without taking ownership of it, e.g. while the daemon
    /// holds it. A missing or unreadable file yields the defaults.
    pub fn peek(path: &Path) -> T {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&mut self) -> io::Result<()> {
        let text = toml::to_string_pretty(&self.data).map_err(io::Error::other)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.set_len(0)?;
        self.file.write_all(text.as_bytes())?;
        self.file.sync_data()
    }
}
