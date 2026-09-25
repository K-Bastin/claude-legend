use super::{Entry, Store};
use crate::paths;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    /// Sessions live in a `claude-legend` folder inside the shared folder.
    pub fn new(folder: &str) -> Self {
        Self {
            root: Path::new(folder).join("claude-legend"),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/')
            .filter(|s| !s.is_empty())
            .fold(self.root.clone(), |p, s| p.join(s))
    }
}

impl Store for LocalStore {
    fn read(&mut self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(path)) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        paths::write_atomic(&self.path(path), data)?;
        Ok(())
    }

    fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        match std::fs::remove_file(self.path(path)) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let entries = match std::fs::read_dir(self.path(dir)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Temp files of write_atomic and of sync tools.
            if name.ends_with(".cl-tmp") || name.starts_with(".syncthing.") {
                continue;
            }
            let meta = entry.metadata()?;
            out.push(Entry {
                name,
                is_dir: meta.is_dir(),
                size: meta.len(),
                mtime: paths::mtime_ms(&entry.path()),
            });
        }
        Ok(out)
    }
}
