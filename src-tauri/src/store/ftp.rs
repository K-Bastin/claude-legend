use super::{ConnectError, Entry, Store};
use anyhow::Context;
use std::io::Cursor;
use std::net::ToSocketAddrs;
use std::time::{Duration, UNIX_EPOCH};
use suppaftp::list::ListParser;
use suppaftp::{FtpError, NativeTlsConnector, NativeTlsFtpStream, Status};

pub struct FtpStore {
    ftp: NativeTlsFtpStream,
    base: String,
    /// MLSD gives exact sizes and dates; LIST is the fallback for old servers.
    mlsd: bool,
}

/// "No such file": 550 per RFC 959, but 450 and 551 are common too.
fn unavailable(e: &FtpError) -> bool {
    matches!(e, FtpError::UnexpectedResponse(r) if matches!(r.status, Status::FileUnavailable | Status::RequestFileActionIgnored | Status::PageTypeUnknown))
}

impl FtpStore {
    pub fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        secure: bool,
        path: &str,
    ) -> Result<Self, ConnectError> {
        let addr = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("adresse invalide : {host}"))?
            .next()
            .with_context(|| format!("hôte introuvable : {host}"))?;
        let mut ftp = NativeTlsFtpStream::connect_timeout(addr, Duration::from_secs(10))
            .with_context(|| format!("connexion à {host}:{port} impossible"))?;
        if secure {
            let tls = native_tls::TlsConnector::new().context("initialisation TLS")?;
            ftp = ftp
                .into_secure(NativeTlsConnector::from(tls), host)
                .context("le serveur refuse FTPS (AUTH TLS)")?;
        }
        ftp.login(user, password).context("identifiants refusés")?;
        ftp.transfer_type(suppaftp::types::FileType::Binary)
            .context("mode binaire refusé")?;
        let mlsd = ftp.feat().map(|f| f.contains_key("MLST")).unwrap_or(false);
        Ok(Self {
            ftp,
            base: path.trim_end_matches('/').to_string(),
            mlsd,
        })
    }

    fn path(&self, rel: &str) -> String {
        super::join(&self.base, rel)
    }

    fn mkdir_all(&mut self, dir: &str) {
        let mut current = String::new();
        for part in dir.split('/') {
            if part.is_empty() {
                if current.is_empty() {
                    current.push('/');
                }
                continue;
            }
            current = super::join(&current, part);
            // Fails when it already exists, which is what we want.
            let _ = self.ftp.mkdir(&current);
        }
    }
}

impl Store for FtpStore {
    fn read(&mut self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match self.ftp.retr_as_buffer(&self.path(path)) {
            Ok(cursor) => Ok(Some(cursor.into_inner())),
            Err(e) if unavailable(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        let target = self.path(path);
        if let Some((parent, _)) = target.rsplit_once('/') {
            self.mkdir_all(parent);
        }
        let (dir, name) = target.rsplit_once('/').unwrap_or(("", &target));
        let tmp = super::join(dir, &format!(".{name}.cl-tmp"));
        self.ftp.put_file(&tmp, &mut Cursor::new(data))?;
        if self.ftp.rename(&tmp, &target).is_err() {
            // Some servers refuse to rename over an existing file.
            let _ = self.ftp.rm(&target);
            self.ftp.rename(&tmp, &target)?;
        }
        Ok(())
    }

    fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        match self.ftp.rm(self.path(path)) {
            Err(e) if !unavailable(&e) => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let path = self.path(dir);
        let lines = match if self.mlsd {
            self.ftp.mlsd(Some(&path))
        } else {
            self.ftp.list(Some(&path))
        } {
            Ok(lines) => lines,
            Err(e) if unavailable(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(lines
            .iter()
            .filter_map(|line| {
                let file = if self.mlsd {
                    ListParser::parse_mlsd(line).ok()?
                } else {
                    ListParser::parse_posix(line)
                        .or_else(|_| ListParser::parse_dos(line))
                        .ok()?
                };
                let name = file.name().to_string();
                (name != "." && name != ".." && !name.ends_with(".cl-tmp")).then(|| Entry {
                    is_dir: file.is_directory(),
                    size: file.size() as u64,
                    mtime: file
                        .modified()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0),
                    name,
                })
            })
            .collect())
    }
}
