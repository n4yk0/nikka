//! Shared HTTP client with a small on-disk cache honouring ETag/Last-Modified.

#[cfg(test)]
pub mod testserver;

use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_BODY: u64 = 32 << 20;
const TIMEOUT: Duration = Duration::from_secs(20);
const STATUS_SNIPPET: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Status { url: String, status: u16, body: String },
    Transport { url: String, message: String },
    Decode { url: String, message: String },
}

impl Error {
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Status { url, status, body } => write!(f, "GET {url}: HTTP {status} {body}"),
            Error::Transport { url, message } => write!(f, "GET {url}: {message}"),
            Error::Decode { url, message } => write!(f, "{url}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Cached {
    pub body: Vec<u8>,
    /// Served from disk because the network failed.
    pub stale: bool,
}

type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    cache_dir: Option<PathBuf>,
    now: Clock,
}

impl Client {
    pub fn new(user_agent: &str, cache_dir: Option<PathBuf>) -> Client {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .user_agent(user_agent)
            .build();
        Client { agent: ureq::Agent::new_with_config(config), cache_dir, now: Arc::new(Timestamp::now) }
    }

    #[cfg(test)]
    pub fn with_clock(mut self, now: impl Fn() -> Timestamp + Send + Sync + 'static) -> Client {
        self.now = Arc::new(now);
        self
    }

    /// Sends a GET (or a form POST when `form` is set) and returns any status; only transport failures are errors.
    pub fn send(&self, url: &str, headers: &[(&str, &str)], form: Option<&[(&str, &str)]>) -> Result<Response, Error> {
        let transport = |e: ureq::Error| Error::Transport { url: redact(url), message: scrub(&e.to_string(), url) };
        let mut response = match form {
            None => {
                let mut req = self.agent.get(url);
                for (k, v) in headers {
                    req = req.header(*k, *v);
                }
                req.call()
            }
            Some(fields) => {
                let mut req = self.agent.post(url);
                for (k, v) in headers {
                    req = req.header(*k, *v);
                }
                req.send_form(fields.iter().copied())
            }
        }
        .map_err(transport)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
            .collect();
        let body = response.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(transport)?;
        Ok(Response { status, headers, body })
    }

    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
        let response = self.send(url, headers, None)?;
        ensure_success(url, response).map(|r| r.body)
    }

    pub fn get_json<T: DeserializeOwned>(&self, url: &str, headers: &[(&str, &str)]) -> Result<T, Error> {
        decode_json(url, &self.get(url, headers)?)
    }

    /// Serves from disk while younger than `ttl`, revalidates afterwards, and falls back to the stale copy when offline.
    pub fn get_cached(&self, url: &str, ttl: Duration) -> Result<Cached, Error> {
        let Some(dir) = &self.cache_dir else {
            return self.get(url, &[]).map(|body| Cached { body, stale: false });
        };
        let key = cache_key(url);
        let body_path = dir.join(format!("{key}.body"));
        let meta_path = dir.join(format!("{key}.json"));
        let now = (self.now)();
        let cached = read_cache(&meta_path, &body_path);
        if let Some((meta, body)) = &cached {
            if meta.age(now).is_some_and(|age| age < SignedDuration::try_from(ttl).unwrap_or(SignedDuration::MAX)) {
                return Ok(Cached { body: body.clone(), stale: false });
            }
        }

        let mut headers = Vec::new();
        if let Some((meta, _)) = &cached {
            if !meta.etag.is_empty() {
                headers.push(("If-None-Match", meta.etag.as_str()));
            }
            if !meta.last_modified.is_empty() {
                headers.push(("If-Modified-Since", meta.last_modified.as_str()));
            }
        }
        let fresh = self
            .send(url, &headers, None)
            .and_then(|r| if r.status == 304 && cached.is_some() { Ok(r) } else { ensure_success(url, r) });
        match (fresh, cached) {
            (Ok(r), Some((mut meta, body))) if r.status == 304 => {
                meta.fetched_at = now.to_string();
                let _ = write_cache(dir, &meta_path, &body_path, &meta, None);
                Ok(Cached { body, stale: false })
            }
            (Err(_), Some((_, body))) => Ok(Cached { body, stale: true }),
            (Err(e), None) => Err(e),
            (Ok(r), _) => {
                let meta = CacheMeta {
                    url: url.to_string(),
                    fetched_at: now.to_string(),
                    etag: r.header("ETag").unwrap_or_default().to_string(),
                    last_modified: r.header("Last-Modified").unwrap_or_default().to_string(),
                };
                let _ = write_cache(dir, &meta_path, &body_path, &meta, Some(&r.body));
                Ok(Cached { body: r.body, stale: false })
            }
        }
    }
}

pub fn decode_json<T: DeserializeOwned>(url: &str, body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|e| Error::Decode { url: redact(url), message: e.to_string() })
}

fn ensure_success(url: &str, r: Response) -> Result<Response, Error> {
    if (200..300).contains(&r.status) {
        return Ok(r);
    }
    let snippet = String::from_utf8_lossy(&r.body[..r.body.len().min(STATUS_SNIPPET)]).trim().to_string();
    Err(Error::Status { url: redact(url), status: r.status, body: snippet })
}

/// Form-encodes pairs sorted by key, so a URL is the same whatever the argument order.
pub fn query_string(pairs: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = pairs.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    sorted.iter().map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v))).collect::<Vec<_>>().join("&")
}

/// Keeps unreserved bytes, percent-encodes the rest, space as `+`.
pub fn query_escape(s: &str) -> String {
    escape_bytes(s, "+")
}

/// Same as `query_escape` for a path segment, space as `%20`.
pub fn path_escape(s: &str) -> String {
    escape_bytes(s, "%20")
}

fn escape_bytes(s: &str, space: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => space.to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Keeps API keys passed as query parameters (Adzuna) out of error messages.
pub fn redact(url: &str) -> String {
    url.split_once('?').map_or(url, |(base, _)| base).to_string()
}

fn scrub(message: &str, url: &str) -> String {
    match url.split_once('?') {
        Some((_, query)) if !query.is_empty() => message.replace(query, "…"),
        _ => message.to_string(),
    }
}

/// First 12 bytes of SHA-256, hex: the names existing ~/.cache/nikka entries already use.
fn cache_key(url: &str) -> String {
    Sha256::digest(url.as_bytes())[..12].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheMeta {
    url: String,
    fetched_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    etag: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    last_modified: String,
}

impl CacheMeta {
    fn age(&self, now: Timestamp) -> Option<SignedDuration> {
        self.fetched_at.parse::<Timestamp>().ok().map(|at| now.duration_since(at))
    }
}

fn read_cache(meta_path: &Path, body_path: &Path) -> Option<(CacheMeta, Vec<u8>)> {
    let meta: CacheMeta = serde_json::from_slice(&fs::read(meta_path).ok()?).ok()?;
    Some((meta, fs::read(body_path).ok()?))
}

fn write_cache(
    dir: &Path,
    meta_path: &Path,
    body_path: &Path,
    meta: &CacheMeta,
    body: Option<&[u8]>,
) -> std::io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let write = |path: &Path, data: &[u8]| {
        fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?.write_all(data)
    };
    if let Some(body) = body {
        write(body_path, body)?;
    }
    write(meta_path, &serde_json::to_vec(meta).map_err(std::io::Error::other)?)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

    use super::testserver::{Response as Reply, TestServer};
    use super::*;

    #[test]
    fn get_sends_user_agent_and_reports_status() {
        let srv = TestServer::start(|req| {
            if req.header("User-Agent") != Some("n4yk0-test") {
                return Reply::status(403, "bad ua");
            }
            if req.path.starts_with("/missing") {
                return Reply::status(404, "404 page not found");
            }
            Reply::ok(r#"{"ok":true}"#)
        });
        let c = Client::new("n4yk0-test", None);
        #[derive(Deserialize)]
        struct Payload {
            ok: bool,
        }
        assert!(c.get_json::<Payload>(&srv.url("/"), &[]).unwrap().ok);
        assert_eq!(c.get(&srv.url("/missing"), &[]).unwrap_err().status(), Some(404));
        let err = c.get(&srv.url("/missing?app_key=s3cr3t"), &[]).unwrap_err();
        assert!(!err.to_string().contains("s3cr3t"), "query string leaked: {err}");
    }

    #[test]
    fn transport_errors_hide_the_query() {
        let err = Client::new("ua", None).get("http://127.0.0.1:1/x?app_key=s3cr3t", &[]).unwrap_err();
        assert!(matches!(err, Error::Transport { .. }));
        assert!(!err.to_string().contains("s3cr3t"), "query string leaked: {err}");
    }

    #[test]
    fn json_errors_name_the_url() {
        let srv = TestServer::start(|_| Reply::ok("<html>"));
        let err = Client::new("ua", None).get_json::<serde_json::Value>(&srv.url("/a?k=s3cr3t"), &[]).unwrap_err();
        assert!(matches!(err, Error::Decode { .. }) && !err.to_string().contains("s3cr3t"), "{err}");
    }

    #[test]
    fn form_posts_and_raw_responses() {
        let srv = TestServer::start(|req| {
            let body = String::from_utf8_lossy(&req.body).into_owned();
            Reply {
                status: 206,
                headers: vec![("Content-Range".into(), "offres 0-1/42".into())],
                body: body.into_bytes(),
            }
        });
        let r = Client::new("ua", None)
            .send(&srv.url("/token"), &[], Some(&[("grant_type", "client_credentials")]))
            .unwrap();
        assert_eq!(r.status, 206);
        assert_eq!(r.header("content-range"), Some("offres 0-1/42"));
        assert_eq!(String::from_utf8(r.body).unwrap(), "grant_type=client_credentials");
    }

    #[test]
    fn cache_lifecycle_fresh_revalidated_stale() {
        let hits = Arc::new(AtomicUsize::new(0));
        let conditional = Arc::new(AtomicUsize::new(0));
        let online = Arc::new(AtomicBool::new(true));
        let (h, c, o) = (hits.clone(), conditional.clone(), online.clone());
        let srv = TestServer::start(move |req| {
            h.fetch_add(1, Ordering::SeqCst);
            if !o.load(Ordering::SeqCst) {
                return Reply::status(502, "down");
            }
            if req.header("If-None-Match") == Some("\"v1\"") {
                c.fetch_add(1, Ordering::SeqCst);
                return Reply { status: 304, headers: vec![], body: vec![] };
            }
            Reply { status: 200, headers: vec![("ETag".into(), "\"v1\"".into())], body: b"payload".to_vec() }
        });
        let clock = Arc::new(AtomicI64::new(1_791_187_200));
        let tick = clock.clone();
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new("ua", Some(dir.path().join("cache")))
            .with_clock(move || Timestamp::from_second(tick.load(Ordering::SeqCst)).unwrap());
        let url = srv.url("/feed");
        let hour = Duration::from_secs(3600);

        let first = client.get_cached(&url, hour).unwrap();
        assert_eq!((first.body.as_slice(), first.stale), (&b"payload"[..], false));

        clock.fetch_add(1800, Ordering::SeqCst);
        client.get_cached(&url, hour).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a fresh cache must not hit the network");

        clock.fetch_add(3600, Ordering::SeqCst);
        let revalidated = client.get_cached(&url, hour).unwrap();
        assert_eq!(revalidated.body, b"payload");
        assert_eq!(conditional.load(Ordering::SeqCst), 1);

        clock.fetch_add(7200, Ordering::SeqCst);
        online.store(false, Ordering::SeqCst);
        let stale = client.get_cached(&url, hour).unwrap();
        assert_eq!((stale.body.as_slice(), stale.stale), (&b"payload"[..], true));
    }

    #[test]
    fn empty_cache_and_failing_server_is_an_error() {
        let srv = TestServer::start(|_| Reply::status(503, "down"));
        let dir = tempfile::tempdir().unwrap();
        assert!(Client::new("ua", Some(dir.path().into())).get_cached(&srv.url("/"), Duration::from_secs(60)).is_err());
    }

    #[test]
    fn cache_files_keep_the_existing_layout() {
        assert_eq!(cache_key("https://example.com/feed"), {
            let digest = Sha256::digest(b"https://example.com/feed");
            digest[..12].iter().map(|b| format!("{b:02x}")).collect::<String>()
        });
        assert_eq!(cache_key("x").len(), 24);
        let go_meta = r#"{"url":"https://x","fetched_at":"2026-10-05T21:50:01.123456789+02:00","etag":"\"abc\""}"#;
        let meta: CacheMeta = serde_json::from_str(go_meta).unwrap();
        let now: Timestamp = "2026-10-05T20:50:01.123456789Z".parse().unwrap();
        assert_eq!(meta.age(now), Some(SignedDuration::from_hours(1)));
        assert_eq!(meta.etag, "\"abc\"");
    }

    #[test]
    fn query_strings_match_go_encoding() {
        let q = query_string(&[
            ("keywords", "développeur"),
            ("location", "Bordeaux, Nouvelle-Aquitaine, France"),
            ("distance", "25"),
        ]);
        assert_eq!(q, "distance=25&keywords=d%C3%A9veloppeur&location=Bordeaux%2C+Nouvelle-Aquitaine%2C+France");
        assert_eq!(query_escape("a b/1~"), "a+b%2F1~");
        assert_eq!(path_escape("214 PMGT/x"), "214%20PMGT%2Fx");
    }

    #[test]
    fn redact_drops_the_query() {
        assert_eq!(redact("https://api/x?app_id=1&app_key=2"), "https://api/x");
        assert_eq!(redact("https://api/x"), "https://api/x");
    }
}
