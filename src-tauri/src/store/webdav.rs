use super::{ConnectError, Entry, Store};
use anyhow::{bail, Context};
use base64::Engine;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use quick_xml::events::Event;
use std::collections::HashSet;
use std::time::{Duration, UNIX_EPOCH};
use ureq::http::{Method, Request};

/// Characters escaped in a URL path segment.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

pub struct WebdavStore {
    agent: ureq::Agent,
    /// Collection URL, always ending with `/`.
    base: String,
    auth: String,
    /// Collections known to exist, to skip MKCOL on every write.
    collections: HashSet<String>,
}

struct Response {
    status: u16,
    body: Vec<u8>,
}

impl WebdavStore {
    pub fn connect(url: &str, user: &str, password: &str) -> Result<Self, ConnectError> {
        let url = url.trim();
        if !url.starts_with("https://") && !url.starts_with("http://") {
            return Err(anyhow::anyhow!("l'adresse doit commencer par https://").into());
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .build(),
            )
            .build()
            .new_agent();
        let mut store = Self {
            agent,
            base: format!("{}/", url.trim_end_matches('/')),
            auth: format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            ),
            collections: HashSet::new(),
        };
        let probe = store.request(
            "PROPFIND",
            &store.base.clone(),
            Some(("0", PROPFIND_BODY.as_bytes())),
        )?;
        match probe.status {
            207 | 200 => {}
            401 | 403 => {
                return Err(anyhow::anyhow!("identifiants refusés par le serveur WebDAV").into())
            }
            404 => {
                let created = store.request("MKCOL", &store.base.clone(), None)?;
                if !matches!(created.status, 201 | 405) {
                    return Err(anyhow::anyhow!(
                        "impossible de créer le dossier distant (HTTP {})",
                        created.status
                    )
                    .into());
                }
            }
            s => {
                return Err(
                    anyhow::anyhow!("réponse inattendue du serveur WebDAV (HTTP {s})").into(),
                )
            }
        }
        store.collections.insert(store.base.clone());
        Ok(store)
    }

    fn url(&self, rel: &str) -> String {
        let encoded: Vec<String> = rel
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| utf8_percent_encode(s, SEGMENT).to_string())
            .collect();
        format!("{}{}", self.base, encoded.join("/"))
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<(&str, &[u8])>,
    ) -> anyhow::Result<Response> {
        let mut builder = Request::builder()
            .method(Method::from_bytes(method.as_bytes())?)
            .uri(url)
            .header("Authorization", &self.auth);
        if let Some((depth, _)) = body.filter(|_| method == "PROPFIND") {
            builder = builder
                .header("Depth", depth)
                .header("Content-Type", "application/xml; charset=utf-8");
        }
        let payload = body.map(|(_, b)| b.to_vec()).unwrap_or_default();
        let mut response = self
            .agent
            .run(builder.body(payload)?)
            .with_context(|| format!("{method} {url}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()?;
        Ok(Response { status, body })
    }

    fn ensure_collections(&mut self, rel_dir: &str) -> anyhow::Result<()> {
        let mut current = String::new();
        for part in rel_dir.split('/').filter(|s| !s.is_empty()) {
            current = super::join(&current, part);
            let url = format!("{}/", self.url(&current));
            if self.collections.contains(&url) {
                continue;
            }
            let response = self.request("MKCOL", &url, None)?;
            if !matches!(response.status, 201 | 405) {
                bail!(
                    "création du dossier distant {current} refusée (HTTP {})",
                    response.status
                );
            }
            self.collections.insert(url);
        }
        Ok(())
    }
}

/// Path component of an URL or href, percent-decoded, without trailing slash.
fn decoded_path(href: &str) -> String {
    let path = match href.find("://") {
        Some(i) => href[i + 3..].find('/').map_or("/", |j| &href[i + 3 + j..]),
        None => href,
    };
    percent_decode_str(path)
        .decode_utf8_lossy()
        .trim_end_matches('/')
        .to_string()
}

fn parse_multistatus(xml: &[u8]) -> anyhow::Result<Vec<(String, Entry)>> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let (mut href, mut is_dir, mut size, mut mtime) = (String::new(), false, 0u64, 0u64);
    let mut text = String::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                text.clear();
                if e.local_name().as_ref() == "response" {
                    (href, is_dir, size, mtime) = (String::new(), false, 0, 0);
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == "collection" {
                    is_dir = true;
                }
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::GeneralRef(r) => {
                if let Some(c) = r.resolve_char_ref()? {
                    text.push(c);
                } else {
                    text.push(match r.xml10_content().as_ref() {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "quot" => '"',
                        _ => '\'',
                    });
                }
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    "href" => href = text.clone(),
                    "getcontentlength" => size = text.trim().parse().unwrap_or(0),
                    "getlastmodified" => {
                        mtime = httpdate::parse_http_date(text.trim())
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_millis() as u64)
                    }
                    "collection" => is_dir = true,
                    "response" => {
                        let path = decoded_path(&href);
                        let name = path.rsplit('/').next().unwrap_or("").to_string();
                        out.push((
                            path,
                            Entry {
                                name,
                                is_dir,
                                size,
                                mtime,
                            },
                        ));
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

impl Store for WebdavStore {
    fn read(&mut self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let response = self.request("GET", &self.url(path), None)?;
        match response.status {
            200 => Ok(Some(response.body)),
            404 => Ok(None),
            s => bail!("lecture de {path} refusée (HTTP {s})"),
        }
    }

    fn write(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()> {
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.ensure_collections(parent)?;
        }
        let response = self.request("PUT", &self.url(path), Some(("", data)))?;
        if !matches!(response.status, 200 | 201 | 204) {
            bail!("écriture de {path} refusée (HTTP {})", response.status);
        }
        Ok(())
    }

    fn delete(&mut self, path: &str) -> anyhow::Result<()> {
        let response = self.request("DELETE", &self.url(path), None)?;
        if !matches!(response.status, 200 | 204 | 404) {
            bail!("suppression de {path} refusée (HTTP {})", response.status);
        }
        Ok(())
    }

    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let url = format!("{}/", self.url(dir).trim_end_matches('/'));
        let response = self.request("PROPFIND", &url, Some(("1", PROPFIND_BODY.as_bytes())))?;
        match response.status {
            207 => {}
            404 => return Ok(Vec::new()),
            s => bail!("liste de {dir} refusée (HTTP {s})"),
        }
        let own = decoded_path(&url);
        Ok(parse_multistatus(&response.body)?
            .into_iter()
            .filter(|(path, e)| *path != own && !e.name.is_empty() && !e.name.ends_with(".cl-tmp"))
            .map(|(_, e)| e)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nextcloud_style_multistatus() {
        let xml = br#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns">
 <d:response><d:href>/remote.php/dav/files/kb/sync/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype>
   <d:getlastmodified>Fri, 25 Sep 2026 12:00:00 GMT</d:getlastmodified></d:prop></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/files/kb/sync/a%20b%26c.json</d:href>
  <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>42</d:getcontentlength>
   <d:getlastmodified>Fri, 25 Sep 2026 12:00:01 GMT</d:getlastmodified></d:prop></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/files/kb/sync/projects/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let entries = parse_multistatus(xml).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, "/remote.php/dav/files/kb/sync");
        assert_eq!(entries[1].1.name, "a b&c.json");
        assert_eq!(entries[1].1.size, 42);
        assert!(!entries[1].1.is_dir && entries[1].1.mtime > 0);
        assert!(entries[2].1.is_dir);
        assert_eq!(
            decoded_path("https://cloud.example.com/remote.php/dav/files/kb/sync/"),
            "/remote.php/dav/files/kb/sync"
        );
    }
}
