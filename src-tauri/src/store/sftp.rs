use super::{ConnectError, Entry, Store};
use crate::config::SftpAuth;
use anyhow::Context;
use base64::Engine;
use ssh2::{ErrorCode, RenameFlags, Session, Sftp};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

const LIBSSH2_FX_NO_SUCH_FILE: i32 = 2;

pub struct SftpStore {
    // Kept alive for the lifetime of the SFTP channel.
    _session: Session,
    sftp: Sftp,
    base: String,
}

fn not_found(e: &ssh2::Error) -> bool {
    matches!(e.code(), ErrorCode::SFTP(LIBSSH2_FX_NO_SUCH_FILE))
}

/// `SHA256:…` fingerprint, formatted like `ssh-keygen -l`.
fn fingerprint(session: &Session) -> anyhow::Result<String> {
    let hash = session
        .host_key_hash(ssh2::HashType::Sha256)
        .context("clé du serveur introuvable")?;
    Ok(format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash)
    ))
}

impl SftpStore {
    #[allow(clippy::too_many_arguments)]
    pub fn connect(
        host: &str,
        port: u16,
        user: &str,
        auth: SftpAuth,
        key_path: Option<&str>,
        secret: Option<&str>,
        path: &str,
        trusted: Option<&str>,
    ) -> Result<Self, ConnectError> {
        let addr = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("adresse invalide : {host}"))?
            .next()
            .with_context(|| format!("hôte introuvable : {host}"))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
            .with_context(|| format!("connexion à {host}:{port} impossible"))?;
        let mut session = Session::new().context("session SSH")?;
        session.set_tcp_stream(tcp);
        session.set_timeout(30_000);
        session.handshake().context("échec de la négociation SSH")?;

        let actual = fingerprint(&session)?;
        match trusted {
            None => {
                return Err(ConnectError::UnknownHost {
                    fingerprint: actual,
                })
            }
            Some(expected) if expected != actual => {
                return Err(ConnectError::HostKeyChanged {
                    expected: expected.to_string(),
                    actual,
                })
            }
            Some(_) => {}
        }

        match auth {
            SftpAuth::Password => session.userauth_password(user, secret.unwrap_or("")),
            SftpAuth::Agent => session.userauth_agent(user),
            SftpAuth::Key => {
                let key = key_path
                    .filter(|k| !k.is_empty())
                    .context("aucune clé privée indiquée")?;
                session.userauth_pubkey_file(
                    user,
                    None,
                    Path::new(key),
                    secret.filter(|s| !s.is_empty()),
                )
            }
        }
        .context("authentification refusée")?;

        let sftp = session.sftp().context("le serveur n'accepte pas SFTP")?;
        Ok(Self {
            _session: session,
            sftp,
            base: path.trim_end_matches('/').to_string(),
        })
    }

    fn path(&self, rel: &str) -> PathBuf {
        PathBuf::from(super::join(&self.base, rel))
    }

    fn mkdir_all(&self, dir: &Path) {
        let mut current = PathBuf::new();
        for part in dir.components() {
            current.push(part);
            // Fails when it already exists, which is what we want.
            let _ = self.sftp.mkdir(&current, 0o755);
        }
    }
}

impl Store for SftpStore {
    fn read(&mut self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut file = match self.sftp.open(self.path(path)) {
            Ok(f) => f,
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        Ok(Some(data))
    }

    fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        let target = self.path(path);
        if let Some(parent) = target.parent() {
            self.mkdir_all(parent);
        }
        let tmp = target.with_file_name(format!(
            ".{}.cl-tmp",
            target
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
        ));
        let mut file = self.sftp.create(&tmp)?;
        file.write_all(data)?;
        drop(file);
        let flags = RenameFlags::OVERWRITE | RenameFlags::ATOMIC | RenameFlags::NATIVE;
        if self.sftp.rename(&tmp, &target, Some(flags)).is_err() {
            // Servers without overwriting rename (SFTP v3).
            let _ = self.sftp.unlink(&target);
            self.sftp.rename(&tmp, &target, None)?;
        }
        Ok(())
    }

    fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        match self.sftp.unlink(&self.path(path)) {
            Err(e) if !not_found(&e) => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let entries = match self.sftp.readdir(self.path(dir)) {
            Ok(entries) => entries,
            Err(e) if not_found(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(entries
            .into_iter()
            .filter_map(|(path, stat)| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                (name != "." && name != ".." && !name.ends_with(".cl-tmp")).then(|| Entry {
                    name,
                    is_dir: stat.is_dir(),
                    size: stat.size.unwrap_or(0),
                    mtime: stat.mtime.unwrap_or(0) * 1000,
                })
            })
            .collect())
    }
}
