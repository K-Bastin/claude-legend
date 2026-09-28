//! Storage backends for synchronisation: a local folder, SFTP, FTP(S) or WebDAV.
//!
//! Every backend exposes the same few file operations on `/`-separated paths
//! relative to its root. [`Remote`] adds a download cache and transparent
//! reconnection on top of them.

mod ftp;
mod local;
pub mod secret;
mod sftp;
mod webdav;

use crate::config::SyncTarget;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time in milliseconds, 0 when unknown.
    pub mtime: u64,
}

pub trait Store: Send {
    /// Reads a file, `None` when it does not exist.
    fn read(&mut self, path: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// Writes a whole file, creating parent directories. Readers never see a
    /// partially written file where the backend allows it.
    fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()>;
    /// Deletes a file; missing files are not an error.
    fn delete(&mut self, path: &str) -> anyhow::Result<()>;
    /// Direct children of a directory, empty when it does not exist.
    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>>;
}

#[derive(Debug)]
pub enum ConnectError {
    /// First connection to an SFTP server: its key must be approved.
    UnknownHost {
        fingerprint: String,
    },
    /// The SFTP server presents a different key than the trusted one.
    HostKeyChanged {
        expected: String,
        actual: String,
    },
    Other(anyhow::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::UnknownHost { fingerprint } => {
                write!(
                    f,
                    "Serveur inconnu (empreinte {fingerprint}) : valide-le dans les réglages."
                )
            }
            ConnectError::HostKeyChanged { expected, actual } => write!(
                f,
                "La clé du serveur a changé ({actual} au lieu de {expected}). \
                 Connexion refusée : vérifie le serveur avant de lui refaire confiance."
            ),
            ConnectError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<anyhow::Error> for ConnectError {
    fn from(e: anyhow::Error) -> Self {
        ConnectError::Other(e)
    }
}

/// Opens a connection to the target. `secret` is the password (or key
/// passphrase) from the keyring.
pub fn connect(target: &SyncTarget, secret: Option<&str>) -> Result<Box<dyn Store>, ConnectError> {
    Ok(match target {
        SyncTarget::None => return Err(anyhow::anyhow!("synchronisation désactivée").into()),
        SyncTarget::Folder { path } => Box::new(local::LocalStore::new(path)),
        SyncTarget::Sftp {
            host,
            port,
            user,
            auth,
            key_path,
            path,
            fingerprint,
        } => Box::new(sftp::SftpStore::connect(
            host,
            *port,
            user,
            *auth,
            key_path.as_deref(),
            secret,
            path,
            fingerprint.as_deref(),
        )?),
        SyncTarget::Ftp {
            host,
            port,
            user,
            secure,
            path,
        } => Box::new(ftp::FtpStore::connect(
            host,
            *port,
            user,
            secret.unwrap_or(""),
            *secure,
            path,
        )?),
        SyncTarget::Webdav { url, user } => Box::new(webdav::WebdavStore::connect(
            url,
            user,
            secret.unwrap_or(""),
        )?),
    })
}

pub fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

/// Connection to the sync target with a download cache.
///
/// Files are only downloaded again when a listing shows a different size or
/// modification time, which keeps a sync cycle to a handful of round trips.
pub struct Remote {
    target: SyncTarget,
    secret: Option<String>,
    conn: Option<Box<dyn Store>>,
    /// Size and mtime seen in listings made since [`Remote::begin`].
    listed: HashMap<String, (u64, u64)>,
    cache: HashMap<String, ((u64, u64), Vec<u8>)>,
}

impl Remote {
    pub fn new(target: SyncTarget, secret: Option<String>) -> Self {
        Self {
            target,
            secret,
            conn: None,
            listed: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    pub fn target(&self) -> &SyncTarget {
        &self.target
    }

    /// Starts an operation: listings from previous operations may be stale, so
    /// reads not preceded by a fresh listing hit the backend.
    pub fn begin(&mut self) {
        self.listed.clear();
    }

    /// Runs `op` on the connection, reconnecting once if a reused connection
    /// turns out to be dead (servers drop idle sessions).
    fn run<T>(
        &mut self,
        mut op: impl FnMut(&mut dyn Store) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let reused = self.conn.is_some();
        if self.conn.is_none() {
            self.conn = Some(
                connect(&self.target, self.secret.as_deref())
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
            );
        }
        match op(self.conn.as_mut().unwrap().as_mut()) {
            Ok(v) => Ok(v),
            Err(_) if reused => {
                self.conn = Some(
                    connect(&self.target, self.secret.as_deref())
                        .map_err(|e| anyhow::anyhow!("{e}"))?,
                );
                op(self.conn.as_mut().unwrap().as_mut()).inspect_err(|_| self.conn = None)
            }
            Err(e) => {
                self.conn = None;
                Err(e)
            }
        }
    }

    pub fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let entries = self.run(|s| s.list(dir))?;
        for e in entries.iter().filter(|e| !e.is_dir) {
            self.listed.insert(join(dir, &e.name), (e.size, e.mtime));
        }
        Ok(entries)
    }

    /// Recursively lists files under `dir`, as paths relative to it.
    pub fn walk(&mut self, dir: &str) -> anyhow::Result<Vec<(String, Entry)>> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
            for entry in self.list(&join(dir, &rel))? {
                let child = join(&rel, &entry.name);
                if entry.is_dir {
                    stack.push(child);
                } else {
                    out.push((child, entry));
                }
            }
        }
        Ok(out)
    }

    pub fn read(&mut self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        if let Some(stamp) = self.listed.get(path).copied() {
            if let Some((cached_stamp, data)) = self.cache.get(path) {
                if *cached_stamp == stamp {
                    return Ok(Some(data.clone()));
                }
            }
            let data = self.run(|s| s.read(path))?;
            if let Some(data) = &data {
                self.cache.insert(path.to_string(), (stamp, data.clone()));
            }
            return Ok(data);
        }
        self.run(|s| s.read(path))
    }

    pub fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        self.listed.remove(path);
        self.cache.remove(path);
        self.run(|s| s.write(path, data))
    }

    pub fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        self.listed.remove(path);
        self.cache.remove(path);
        self.run(|s| s.delete(path))
    }
}

/// Checks that the target is reachable and writable.
pub fn test(target: &SyncTarget, secret: Option<&str>) -> Result<(), ConnectError> {
    let mut store = connect(target, secret)?;
    let probe = format!(".claude-legend-test-{}", uuid::Uuid::new_v4());
    store.write(&probe, b"ok")?;
    let back = store.read(&probe)?;
    store.delete(&probe)?;
    if back.as_deref() != Some(b"ok".as_slice()) {
        return Err(anyhow::anyhow!("le fichier de test relu ne correspond pas").into());
    }
    store.list("")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs against a real server described by `CL_TEST_TARGET` (JSON of a
    /// `SyncTarget`) and `CL_TEST_SECRET`:
    /// `CL_TEST_TARGET='{"kind":"webdav",…}' cargo test -- --ignored remote_store`
    #[test]
    #[ignore]
    fn remote_store_roundtrip() {
        let target: SyncTarget =
            serde_json::from_str(&std::env::var("CL_TEST_TARGET").unwrap()).unwrap();
        let secret = std::env::var("CL_TEST_SECRET").ok();
        test(&target, secret.as_deref()).unwrap_or_else(|e| panic!("{e}"));

        let mut remote = Remote::new(target, secret);
        let dir = format!("it-{}", uuid::Uuid::new_v4());
        let file = format!("{dir}/a b/é&x.json");
        let big = vec![b'x'; 12 * 1024 * 1024];
        remote.write(&file, b"v1").unwrap();
        remote.write(&format!("{dir}/big.bin"), &big).unwrap();
        remote.write(&file, b"v2 longer").unwrap();
        assert_eq!(
            remote.read(&file).unwrap().as_deref(),
            Some(b"v2 longer".as_slice())
        );
        assert_eq!(
            remote
                .read(&format!("{dir}/big.bin"))
                .unwrap()
                .map(|d| d.len()),
            Some(big.len())
        );
        assert_eq!(remote.read(&format!("{dir}/missing")).unwrap(), None);
        assert!(remote.list(&format!("{dir}/nope")).unwrap().is_empty());

        let mut files: Vec<(String, u64)> = remote
            .walk(&dir)
            .unwrap()
            .into_iter()
            .map(|(rel, e)| (rel, e.size))
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                ("a b/é&x.json".to_string(), 9),
                ("big.bin".to_string(), big.len() as u64)
            ]
        );
        // Served from the cache after a listing.
        assert_eq!(
            remote.read(&file).unwrap().as_deref(),
            Some(b"v2 longer".as_slice())
        );

        remote.delete(&file).unwrap();
        remote.delete(&format!("{dir}/big.bin")).unwrap();
        remote.delete(&file).unwrap();
        assert_eq!(remote.read(&file).unwrap(), None);
    }
}
