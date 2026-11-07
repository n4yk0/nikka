//! Aggregates RSS/JSON sources per category and ranks stories by recency, editorial position and cross-source coverage.

mod defaults;
mod extras;
pub mod feed;
mod rank;
mod sources;

use std::thread;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::httpx::Client;

pub use defaults::default_categories;
pub use extras::{KEV_URL, Kev, LAUNCHES_URL, Launch, fetch_kev, fetch_launches, parse_kev, parse_launches};
pub use rank::{RankOptions, rank, tokens};
pub use sources::{feed_items, google_news_items};

pub const KIND_RSS: &str = "rss";
pub const KIND_GOOGLE_NEWS: &str = "googlenews";
pub const KIND_SPACEFLIGHT: &str = "spaceflight";
pub const KIND_HACKER_NEWS: &str = "hn";

pub const EXTRA_LAUNCHES: &str = "spacex-launches";
pub const EXTRA_KEV: &str = "cisa-kev";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub weight: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub editorial: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skip_category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub require: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Category {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub half_life_hours: f64,
    #[serde(default)]
    pub max_age_hours: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub extra: String,
    #[serde(default)]
    pub sources: Vec<Source>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub title: String,
    pub link: String,
    pub summary: String,
    pub feed: String,
    pub publisher: String,
    pub published: Option<Timestamp>,
    /// Index in the source listing, meaningful for editorial front pages.
    pub position: usize,
    pub boost: f64,
    pub weight: f64,
    pub editorial: bool,
    pub score: f64,
    /// Number of distinct feeds that ran the same story.
    pub coverage: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub category: Category,
    pub items: Vec<Item>,
    pub failures: Vec<String>,
}

/// Fetches every source of the category in parallel; a failing source only adds to `failures`.
pub fn fetch(client: &Client, category: &Category, limit: usize, now: Timestamp) -> Report {
    let results: Vec<Result<Vec<Item>, String>> = thread::scope(|s| {
        let handles: Vec<_> =
            category.sources.iter().map(|src| s.spawn(move || sources::fetch_source(client, src))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("fetch panicked".to_string()))).collect()
    });
    let mut all = Vec::new();
    let mut failures = Vec::new();
    for (src, result) in category.sources.iter().zip(results) {
        match result {
            Ok(items) => all.extend(items),
            Err(e) => failures.push(format!("{}: {e}", src.name)),
        }
    }
    let items = rank(
        all,
        &RankOptions {
            now,
            half_life: hours(category.half_life_hours, 24.0),
            max_age: hours(category.max_age_hours, 72.0),
            limit,
        },
    );
    Report { category: category.clone(), items, failures }
}

fn hours(h: f64, fallback: f64) -> SignedDuration {
    let h = if h > 0.0 { h } else { fallback };
    SignedDuration::from_secs_f64(h * 3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::httpx::testserver::{Response, TestServer};

    const SAMPLE_RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>t</title>
<item><title>Open article &amp; more</title><link>https://ex/1</link><pubDate>Mon, 05 Oct 2026 08:00:00 +0000</pubDate></item>
</channel></rss>"#;

    #[test]
    fn fetch_collects_items_and_failures_per_source() {
        let srv = TestServer::start(|req| match req.path.as_str() {
            "/rss" => Response::ok(SAMPLE_RSS),
            "/hn" => Response::ok(
                r#"{"hits":[{"title":"Ask HN: LLM agents in prod?","url":null,"points":120,"objectID":"42","created_at_i":1791194400,"num_comments":7},{"title":"Unrelated","url":"https://u"}]}"#,
            ),
            "/sfn" => Response::ok(
                r#"{"results":[{"title":"Starship","url":"https://s","news_site":"SpaceNews","summary":"s","published_at":"2026-10-05T09:00:00Z"}]}"#,
            ),
            _ => Response::status(500, "x"),
        });
        let source = |name: &str, kind: &str, path: &str| Source {
            name: name.to_string(),
            kind: kind.to_string(),
            url: srv.url(path),
            ..Source::default()
        };
        let category = Category {
            key: "t".into(),
            max_age_hours: 1000.0,
            sources: vec![
                source("rss", "", "/rss"),
                Source {
                    filter: r"\b(llms?|agents?)\b".into(),
                    weight: 1.1,
                    editorial: true,
                    ..source("hn", KIND_HACKER_NEWS, "/hn")
                },
                source("sfn", KIND_SPACEFLIGHT, "/sfn"),
                source("down", "", "/down"),
                source("bogus", "nope", "/"),
            ],
            ..Category::default()
        };
        let now: Timestamp = "2026-10-05T12:00:00Z".parse().unwrap();
        let report = fetch(&Client::new("test", None), &category, 10, now);
        assert_eq!(report.failures.len(), 2, "{:?}", report.failures);
        assert!(
            report.failures[0].starts_with("down: GET ") && report.failures[1] == "bogus: unknown source kind \"nope\""
        );

        let by_link = |link: &str| report.items.iter().find(|i| i.link == link).cloned();
        let hn = by_link("https://news.ycombinator.com/item?id=42").expect("hn item");
        assert_eq!(
            (hn.publisher.as_str(), hn.feed.as_str(), hn.weight, hn.editorial),
            ("Hacker News", "hn", 1.1, true)
        );
        assert!(hn.boost > 1.0 && hn.summary == "120 points · 7 comments");
        assert!(by_link("https://u").is_none(), "the HN filter must drop unrelated titles");
        let sfn = by_link("https://s").expect("spaceflight item");
        assert_eq!((sfn.publisher.as_str(), sfn.feed.as_str(), sfn.weight, sfn.boost), ("SpaceNews", "sfn", 1.0, 1.0));
        let rss = by_link("https://ex/1").expect("rss item");
        assert_eq!((rss.title.as_str(), rss.publisher.as_str()), ("Open article & more", "rss"));
    }

    #[test]
    fn fallback_windows() {
        assert_eq!(hours(0.0, 24.0), SignedDuration::from_hours(24));
        assert_eq!(hours(1.5, 24.0), SignedDuration::from_mins(90));
    }
}
