//! Storage backends for synchronisation: a local folder, SFTP, FTP(S) or WebDAV.
//!
//! Every backend exposes the same few file operations on `/`-separated paths
//! relative to its root. [`Remote`] adds a download cache, transparent
//! reconnection and end-to-end encryption (see [`crypto`]) on top of them.

pub mod crypto;
mod ftp;
mod local;
pub mod secret;
mod sftp;
mod webdav;

use crate::config::SyncTarget;
use anyhow::{anyhow, bail, Context};
use crypto::{is_sealed, Cipher, Marker, MARKER, OVERHEAD};
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

/// Whether the target is encrypted, and whether this machine can read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionStatus {
    pub enabled: bool,
    pub unlocked: bool,
}

/// Connection to the sync target with a download cache.
///
/// Files are only downloaded again when a listing shows a different size or
/// modification time, which keeps a sync cycle to a handful of round trips.
/// When the target holds an encryption marker, contents are sealed on write
/// and opened on read, and listings report plain sizes.
pub struct Remote {
    target: SyncTarget,
    secret: Option<String>,
    /// Encryption passphrase kept in this machine's keyring.
    passphrase: Option<String>,
    conn: Option<Box<dyn Store>>,
    /// Size and mtime seen in listings made since [`Remote::begin`].
    listed: HashMap<String, (u64, u64)>,
    /// Plain contents.
    cache: HashMap<String, ((u64, u64), Vec<u8>)>,
    /// Cipher of the target, looked up once per operation; `None` until then.
    cipher: Option<Option<Cipher>>,
    /// Last marker unlocked, so the key is only derived once.
    unlocked: Option<(Marker, Cipher)>,
}

impl Remote {
    pub fn new(target: SyncTarget, secret: Option<String>, passphrase: Option<String>) -> Self {
        Self {
            target,
            secret,
            passphrase,
            conn: None,
            listed: HashMap::new(),
            cache: HashMap::new(),
            cipher: None,
            unlocked: None,
        }
    }

    pub fn target(&self) -> &SyncTarget {
        &self.target
    }

    /// Starts an operation: listings from previous operations may be stale, so
    /// reads not preceded by a fresh listing hit the backend.
    pub fn begin(&mut self) {
        self.listed.clear();
        // Another machine may have turned encryption on or off.
        self.cipher = None;
    }

    fn read_marker(&mut self) -> anyhow::Result<Option<Marker>> {
        self.run(|s| s.read(MARKER))?
            .map(|data| serde_json::from_slice(&data).context("marqueur de chiffrement illisible"))
            .transpose()
    }

    fn unlock(&mut self, marker: &Marker) -> anyhow::Result<Cipher> {
        if let Some((known, cipher)) = &self.unlocked {
            if known == marker {
                return Ok(cipher.clone());
            }
        }
        let passphrase = self.passphrase.as_deref().ok_or_else(|| {
            anyhow!("les conversations de la cible sont chiffrées : saisis la phrase de passe dans les réglages de ce PC")
        })?;
        let cipher = marker.unlock(passphrase)?;
        self.unlocked = Some((marker.clone(), cipher.clone()));
        Ok(cipher)
    }

    /// The cipher of the target, `None` when it is not encrypted.
    fn cipher(&mut self) -> anyhow::Result<Option<Cipher>> {
        if let Some(cipher) = &self.cipher {
            return Ok(cipher.clone());
        }
        let cipher = match self.read_marker()? {
            Some(marker) => Some(self.unlock(&marker)?),
            None => None,
        };
        self.cipher = Some(cipher.clone());
        Ok(cipher)
    }

    /// Plain content of a file read from the target.
    fn open(&mut self, path: &str, data: Vec<u8>) -> anyhow::Result<Vec<u8>> {
        match self.cipher()? {
            Some(cipher) if is_sealed(&data) => {
                cipher.open(&data).with_context(|| path.to_string())
            }
            // Written before encryption was turned on; sealed at its next write.
            Some(_) => Ok(data),
            // Never taken for a plain (hence unreadable, hence missing) file.
            None if is_sealed(&data) => {
                bail!("{path} est chiffré mais la cible n'a pas de marqueur de chiffrement")
            }
            None => Ok(data),
        }
    }

    pub fn encryption_status(&mut self) -> anyhow::Result<EncryptionStatus> {
        self.begin();
        let Some(marker) = self.read_marker()? else {
            return Ok(EncryptionStatus {
                enabled: false,
                unlocked: true,
            });
        };
        Ok(EncryptionStatus {
            enabled: true,
            unlocked: self.unlock(&marker).is_ok(),
        })
    }

    /// Turns encryption on (writing the marker first, so an interrupted run
    /// resumes with the same key) or off (removing the marker last), then
    /// rewrites every file accordingly. Returns the number of files rewritten.
    pub fn set_encryption(&mut self, enable: bool) -> anyhow::Result<u32> {
        self.begin();
        let marker = self.read_marker()?;
        let cipher = match (enable, marker) {
            (true, Some(marker)) => self.unlock(&marker)?,
            (true, None) => {
                let passphrase = self
                    .passphrase
                    .clone()
                    .ok_or_else(|| anyhow!("indique une phrase de passe"))?;
                let (marker, cipher) = Marker::create(&passphrase)?;
                let json = serde_json::to_vec_pretty(&marker)?;
                self.run(|s| s.write(MARKER, &json))?;
                self.unlocked = Some((marker, cipher.clone()));
                cipher
            }
            (false, Some(marker)) => self.unlock(&marker)?,
            (false, None) => return Ok(0),
        };
        self.cipher = Some(Some(cipher.clone()));
        let mut rewritten = 0;
        for (path, _) in self.walk("")? {
            if path.trim_start_matches('.').starts_with(TEST_FILE_PREFIX) {
                continue;
            }
            let Some(data) = self.run(|s| s.read(&path))? else {
                continue;
            };
            let new = match (enable, is_sealed(&data)) {
                (true, false) => cipher.seal(&data),
                (false, true) => cipher.open(&data).with_context(|| path.clone())?,
                _ => continue,
            };
            self.run(|s| s.write(&path, &new))?;
            rewritten += 1;
        }
        if !enable {
            self.run(|s| s.delete(MARKER))?;
        }
        self.cache.clear();
        self.begin();
        Ok(rewritten)
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
        let mut entries = self.run(|s| s.list(dir))?;
        if dir.is_empty() {
            entries.retain(|e| e.name != MARKER);
        }
        if self.cipher()?.is_some() {
            // Plain sizes, which the sync compares with local files. A file
            // written before encryption looks smaller and gets rewritten.
            for e in entries.iter_mut().filter(|e| !e.is_dir) {
                e.size = e.size.saturating_sub(OVERHEAD);
            }
        }
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
            let data = match self.run(|s| s.read(path))? {
                Some(data) => Some(self.open(path, data)?),
                None => None,
            };
            if let Some(data) = &data {
                self.cache.insert(path.to_string(), (stamp, data.clone()));
            }
            return Ok(data);
        }
        match self.run(|s| s.read(path))? {
            Some(data) => Ok(Some(self.open(path, data)?)),
            None => Ok(None),
        }
    }

    pub fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        self.listed.remove(path);
        self.cache.remove(path);
        let data = match self.cipher()? {
            Some(cipher) => cipher.seal(data),
            None => data.to_vec(),
        };
        self.run(|s| s.write(path, &data))
    }

    pub fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        self.listed.remove(path);
        self.cache.remove(path);
        self.run(|s| s.delete(path))
    }
}

/// Name of the file written by [`test`]. Older versions prefixed it with a dot,
/// which NAS FTP servers often refuse.
const TEST_FILE_PREFIX: &str = "claude-legend-test-";

/// Checks that the target is reachable and writable.
pub fn test(target: &SyncTarget, secret: Option<&str>) -> Result<(), ConnectError> {
    let mut store = connect(target, secret)?;
    let probe = format!("{TEST_FILE_PREFIX}{}", uuid::Uuid::new_v4());
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

    /// Turning encryption on seals every file, a wrong passphrase is refused,
    /// and turning it off brings plain files back without the marker.
    #[test]
    fn encryption_can_be_turned_on_and_off() {
        let dir = std::env::temp_dir().join(format!("cl-crypt-{}", uuid::Uuid::new_v4()));
        let target = SyncTarget::Folder {
            path: dir.to_string_lossy().into_owned(),
        };
        let on_disk = |rel: &str| std::fs::read(dir.join("claude-legend").join(rel)).unwrap();
        let pass = Some("correct horse battery".to_string());

        let mut remote = Remote::new(target.clone(), None, pass.clone());
        remote
            .write("projects/a/project.json", b"{\"key\":\"a\"}")
            .unwrap();
        assert_eq!(remote.set_encryption(true).unwrap(), 1);
        assert!(crypto::is_sealed(&on_disk("projects/a/project.json")));
        remote.write("locks/x.json", b"{}").unwrap();
        assert!(crypto::is_sealed(&on_disk("locks/x.json")));
        remote.begin();
        let listed = remote.list("projects/a").unwrap();
        assert_eq!(listed[0].size, 11, "plain size");
        assert_eq!(
            remote.read("projects/a/project.json").unwrap().as_deref(),
            Some(b"{\"key\":\"a\"}".as_slice())
        );
        assert!(remote
            .list("")
            .unwrap()
            .iter()
            .all(|e| e.name != crypto::MARKER));

        let mut wrong = Remote::new(target.clone(), None, Some("wrong horse battery".into()));
        assert!(!wrong.encryption_status().unwrap().unlocked);
        assert!(wrong.read("locks/x.json").is_err());

        assert_eq!(remote.set_encryption(false).unwrap(), 2);
        assert_eq!(on_disk("projects/a/project.json"), b"{\"key\":\"a\"}");
        assert!(!dir.join("claude-legend").join(crypto::MARKER).exists());
        let mut plain = Remote::new(target, None, None);
        assert!(!plain.encryption_status().unwrap().enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

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

        let mut remote = Remote::new(target, secret, None);
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
