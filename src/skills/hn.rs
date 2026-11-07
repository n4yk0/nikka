use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;

use super::Corpus;
use crate::httpx::Client;
use crate::textutil::clean_text;

pub const HN_THREADS_URL: &str = "https://hn.algolia.com/api/v1/search_by_date?query=%22Ask%20HN%3A%20Who%20is%20hiring%22&tags=story,author_whoishiring&hitsPerPage=6";
pub const HN_ITEM_URL: &str = "https://hn.algolia.com/api/v1/items/";
const HIRING_PREFIX: &str = "Ask HN: Who is hiring?";
const THREADS_TTL: Duration = Duration::from_secs(6 * 3600);
const CURRENT_TTL: Duration = Duration::from_secs(6 * 3600);
const PAST_THREAD_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
const MIN_POST_BYTES: usize = 80;

#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub created: Option<Timestamp>,
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    hits: Vec<SearchHit>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchHit {
    #[serde(rename = "objectID")]
    object_id: String,
    title: Option<String>,
    created_at_i: i64,
}

/// "Who is hiring?" threads newest first; the whoishiring account also posts freelancer and "wants to be hired" threads.
pub fn hiring_threads(data: &[u8]) -> Result<Vec<Thread>, serde_json::Error> {
    let search: Search = serde_json::from_slice(data)?;
    Ok(search
        .hits
        .into_iter()
        .filter_map(|h| {
            let title = h.title.filter(|t| t.starts_with(HIRING_PREFIX))?;
            Some(Thread { id: h.object_id, title, created: Timestamp::from_second(h.created_at_i).ok() })
        })
        .collect())
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    children: Vec<Comment>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Comment {
    text: Option<String>,
    author: Option<String>,
}

/// Top-level job posts; deleted comments (no author) and one-liners are dropped.
pub fn thread_posts(data: &[u8]) -> Result<Vec<String>, serde_json::Error> {
    let item: Item = serde_json::from_slice(data)?;
    Ok(item
        .children
        .into_iter()
        .filter(|c| c.author.is_some())
        .filter_map(|c| c.text.map(|t| clean_text(&t)))
        .filter(|t| t.len() >= MIN_POST_BYTES)
        .collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct HiringCorpora {
    pub current: Corpus,
    /// The previous month, used as the trend baseline.
    pub previous: Option<Corpus>,
}

/// The latest monthly thread and, when available, the previous one.
pub fn fetch_hn(client: &Client, threads_url: &str, item_url: &str) -> Result<HiringCorpora, String> {
    let listing = client.get_cached(threads_url, THREADS_TTL).map_err(|e| e.to_string())?;
    let threads = hiring_threads(&listing.body).map_err(|e| e.to_string())?;
    let Some(latest) = threads.first() else { return Err(format!("no {HIRING_PREFIX:?} thread found")) };
    let current = fetch_thread(client, item_url, latest, CURRENT_TTL)?;
    let previous = threads.get(1).and_then(|t| fetch_thread(client, item_url, t, PAST_THREAD_TTL).ok());
    Ok(HiringCorpora { current, previous })
}

fn fetch_thread(client: &Client, item_url: &str, thread: &Thread, ttl: Duration) -> Result<Corpus, String> {
    let url = format!("{item_url}{}", thread.id);
    let cached = client.get_cached(&url, ttl).map_err(|e| e.to_string())?;
    let docs = thread_posts(&cached.body).map_err(|e| format!("{url}: {e}"))?;
    let month = thread.title.trim_start_matches(HIRING_PREFIX).trim().trim_matches(['(', ')']);
    Ok(Corpus { name: "hn".to_string(), label: format!("HN hiring {month}"), docs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::httpx::testserver::{Response, TestServer};

    const THREADS: &str = r#"{"hits":[
        {"objectID":"3","title":"Ask HN: Who wants to be hired? (October 2026)","created_at_i":1790000000},
        {"objectID":"2","title":"Ask HN: Who is hiring? (October 2026)","created_at_i":1790000000},
        {"objectID":"1","title":"Ask HN: Who is hiring? (September 2026)","created_at_i":1787000000}]}"#;

    fn post(text: &str) -> String {
        format!(r#"{{"text":"{text}","author":"a"}}"#)
    }

    #[test]
    fn only_hiring_threads_are_kept() {
        let threads = hiring_threads(THREADS.as_bytes()).unwrap();
        assert_eq!(threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert!(hiring_threads(b"nope").is_err());
    }

    #[test]
    fn posts_skip_deleted_and_short_comments() {
        let long = "Acme | Senior Go Engineer | Remote | ".repeat(4);
        let data = format!(
            r#"{{"children":[{},{{"text":null,"author":null}},{{"text":"deleted but long enough to pass the length check, really long enough","author":null}},{}]}}"#,
            post(&format!("{long}&#x2F;<p>Python</p>")),
            post("too short")
        );
        let posts = thread_posts(data.as_bytes()).unwrap();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(posts[0].contains("/ Python"), "{posts:?}");
    }

    #[test]
    fn fetch_returns_current_and_previous_month() {
        let long = "Acme | Rust and Python engineers | Remote | Paris | ".repeat(3);
        let item = format!(r#"{{"children":[{}]}}"#, post(&long));
        let srv = TestServer::start(move |req| match req.path.as_str() {
            "/threads" => Response::ok(THREADS),
            "/items/2" | "/items/1" => Response::ok(&item),
            _ => Response::status(404, "missing"),
        });
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new("test", Some(dir.path().into()));
        let HiringCorpora { current, previous } = fetch_hn(&client, &srv.url("/threads"), &srv.url("/items/")).unwrap();
        assert_eq!(
            (current.name.as_str(), current.label.as_str(), current.docs.len()),
            ("hn", "HN hiring October 2026", 1)
        );
        assert_eq!(previous.map(|p| p.label), Some("HN hiring September 2026".to_string()));

        let none = TestServer::start(|_| Response::ok(r#"{"hits":[]}"#));
        assert!(fetch_hn(&client, &none.url("/empty"), &none.url("/items/")).unwrap_err().contains("no \"Ask HN"));
    }
}
