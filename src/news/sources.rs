use std::collections::HashMap;

use jiff::Timestamp;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use super::feed::{self, Entry};
use super::{Item, KIND_GOOGLE_NEWS, KIND_HACKER_NEWS, KIND_RSS, KIND_SPACEFLIGHT, Source};
use crate::httpx::{Client, query_escape};
use crate::textutil::{clean_text, truncate};

const SUMMARY_CHARS: usize = 220;

pub(super) fn fetch_source(client: &Client, src: &Source) -> Result<Vec<Item>, String> {
    let mut items = match src.kind.as_str() {
        "" | KIND_RSS => feed_items(&fetch_feed(client, src)?, src)?,
        KIND_GOOGLE_NEWS => google_news_items(&fetch_feed(client, src)?, src)?,
        KIND_SPACEFLIGHT => spaceflight_items(client.get_json(&src.url, &[]).map_err(|e| e.to_string())?),
        KIND_HACKER_NEWS => hacker_news_items(client.get_json(&src.url, &[]).map_err(|e| e.to_string())?, src)?,
        other => return Err(format!("unknown source kind {other:?}")),
    };
    let weight = if src.weight > 0.0 { src.weight } else { 1.0 };
    for item in &mut items {
        item.feed = src.name.clone();
        if item.publisher.is_empty() {
            item.publisher = src.name.clone();
        }
        item.weight = weight;
        item.editorial = src.editorial;
        if item.boost == 0.0 {
            item.boost = 1.0;
        }
    }
    Ok(items)
}

fn fetch_feed(client: &Client, src: &Source) -> Result<Vec<Entry>, String> {
    let body = client.get(&src.url, &[]).map_err(|e| e.to_string())?;
    feed::parse(&String::from_utf8_lossy(&body))
}

fn compile_filter(expr: &str) -> Result<Option<Regex>, String> {
    if expr.is_empty() {
        return Ok(None);
    }
    RegexBuilder::new(expr).case_insensitive(true).build().map(Some).map_err(|e| format!("invalid filter: {e}"))
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.to_lowercase()
}

pub fn feed_items(entries: &[Entry], src: &Source) -> Result<Vec<Item>, String> {
    let filter = compile_filter(&src.filter)?;
    let require = src.require.split_once('=');
    let mut items = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        if !src.skip_category.is_empty() && entry.categories.iter().any(|c| same_text(c, &src.skip_category)) {
            continue;
        }
        if let Some((key, value)) = require {
            if !same_text(entry.custom.get(key).map_or("", String::as_str), value) {
                continue;
            }
        }
        let title = clean_text(&entry.title);
        if title.is_empty() || filter.as_ref().is_some_and(|f| !f.is_match(&title)) {
            continue;
        }
        items.push(Item {
            title,
            link: entry.link.trim().to_string(),
            summary: truncate(&clean_text(&entry.description), SUMMARY_CHARS),
            published: entry.time(),
            position,
            ..Item::default()
        });
    }
    Ok(items)
}

/// Splits the " - Publisher" title suffix and uses the size of the story cluster
/// listed in the description as an importance boost.
pub fn google_news_items(entries: &[Entry], src: &Source) -> Result<Vec<Item>, String> {
    let descriptions: HashMap<&str, &str> = entries.iter().map(|e| (e.link.trim(), e.description.as_str())).collect();
    let mut items = feed_items(entries, src)?;
    for item in &mut items {
        if let Some(idx) = item.title.rfind(" - ").filter(|&i| i > 0) {
            item.publisher = item.title[idx + 3..].trim().to_string();
            item.title = item.title[..idx].trim().to_string();
        }
        let related = descriptions.get(item.link.as_str()).map_or(0, |d| d.matches("<li>").count());
        item.boost = 1.0 + 0.1 * related.saturating_sub(1).min(5) as f64;
        item.summary.clear();
    }
    Ok(items)
}

#[derive(Deserialize)]
struct SpaceflightResponse {
    #[serde(default)]
    results: Vec<SpaceflightArticle>,
}

#[derive(Deserialize)]
struct SpaceflightArticle {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    news_site: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

fn spaceflight_items(resp: SpaceflightResponse) -> Vec<Item> {
    resp.results
        .into_iter()
        .map(|r| Item {
            title: clean_text(&r.title),
            link: r.url,
            summary: truncate(&clean_text(r.summary.as_deref().unwrap_or_default()), SUMMARY_CHARS),
            publisher: r.news_site,
            published: r.published_at.and_then(|p| p.parse::<Timestamp>().ok()),
            ..Item::default()
        })
        .collect()
}

#[derive(Deserialize)]
struct HnResponse {
    #[serde(default)]
    hits: Vec<HnHit>,
}

#[derive(Deserialize)]
struct HnHit {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default, rename = "objectID")]
    object_id: String,
    #[serde(default)]
    created_at_i: i64,
    #[serde(default)]
    num_comments: Option<i64>,
}

fn hacker_news_items(resp: HnResponse, src: &Source) -> Result<Vec<Item>, String> {
    let filter = compile_filter(&src.filter)?;
    let mut items = Vec::new();
    for (position, hit) in resp.hits.into_iter().enumerate() {
        let title = hit.title.unwrap_or_default();
        if filter.as_ref().is_some_and(|f| !f.is_match(&title)) {
            continue;
        }
        let points = hit.points.unwrap_or(0);
        let link = hit
            .url
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| format!("https://news.ycombinator.com/item?id={}", query_escape(&hit.object_id)));
        items.push(Item {
            title: clean_text(&title),
            link,
            summary: format!("{points} points · {} comments", hit.num_comments.unwrap_or(0)),
            publisher: "Hacker News".to_string(),
            published: Timestamp::from_second(hit.created_at_i).ok(),
            position,
            boost: 1.0 + (points.max(0) as f64 + 1.0).log10() / 4.0,
            ..Item::default()
        });
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>t</title>
<item><title>Open article &amp; more</title><link>https://ex/1</link><pubDate>Mon, 05 Oct 2026 08:00:00 +0000</pubDate><description>&lt;p&gt;Hello&lt;/p&gt;</description><free>true</free></item>
<item><title>Paywalled article</title><link>https://ex/2</link><pubDate>Mon, 05 Oct 2026 09:00:00 +0000</pubDate><free>false</free></item>
<item><title>Sponsored thing</title><link>https://ex/3</link><category>sponsored</category><free>true</free></item>
</channel></rss>"#;

    const SAMPLE_GOOGLE_NEWS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>g</title>
<item><title>Big story here - Le Monde.fr</title><link>https://news.google.com/rss/articles/abc</link>
<description>&lt;ol&gt;&lt;li&gt;a&lt;/li&gt;&lt;li&gt;b&lt;/li&gt;&lt;li&gt;c&lt;/li&gt;&lt;/ol&gt;</description>
<pubDate>Mon, 05 Oct 2026 10:00:00 GMT</pubDate></item>
<item><title>No publisher suffix</title><link>https://news.google.com/rss/articles/def</link></item>
</channel></rss>"#;

    fn entries(xml: &str) -> Vec<Entry> {
        feed::parse(xml).unwrap()
    }

    #[test]
    fn feed_items_apply_require_skip_category_and_filter() {
        let src = Source { require: "free=true".into(), skip_category: "Sponsored".into(), ..Source::default() };
        let items = feed_items(&entries(SAMPLE_RSS), &src).unwrap();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!((items[0].title.as_str(), items[0].summary.as_str()), ("Open article & more", "Hello"));
        assert_eq!(items[0].published, Some("2026-10-05T08:00:00Z".parse().unwrap()));

        assert!(feed_items(&entries(SAMPLE_RSS), &Source { filter: "(".into(), ..Source::default() }).is_err());
        let filtered =
            feed_items(&entries(SAMPLE_RSS), &Source { filter: "paywalled".into(), ..Source::default() }).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!((filtered[0].link.as_str(), filtered[0].position), ("https://ex/2", 1));
    }

    #[test]
    fn google_news_splits_publishers_and_boosts_clusters() {
        let items = google_news_items(&entries(SAMPLE_GOOGLE_NEWS), &Source::default()).unwrap();
        assert_eq!(
            (items[0].title.as_str(), items[0].publisher.as_str(), items[0].boost),
            ("Big story here", "Le Monde.fr", 1.2)
        );
        assert_eq!(
            (items[1].title.as_str(), items[1].publisher.as_str(), items[1].boost),
            ("No publisher suffix", "", 1.0)
        );
        assert!(items.iter().all(|i| i.summary.is_empty()));
    }

    #[test]
    fn hacker_news_links_and_boosts() {
        let resp: HnResponse = serde_json::from_str(
            r#"{"hits":[{"title":"Show HN: x","url":"","points":-3,"objectID":"a b/1","created_at_i":0},{"title":null}]}"#,
        )
        .unwrap();
        let items = hacker_news_items(resp, &Source::default()).unwrap();
        assert_eq!(items[0].link, "https://news.ycombinator.com/item?id=a+b%2F1");
        assert_eq!(items[0].boost, 1.0);
        assert_eq!(items[0].summary, "-3 points · 0 comments");
    }

    #[test]
    fn spaceflight_tolerates_missing_fields() {
        let resp: SpaceflightResponse =
            serde_json::from_str(r#"{"results":[{"title":"T &amp; U","url":"https://s","news_site":"NSF","summary":null,"published_at":"bad"}]}"#)
                .unwrap();
        let items = spaceflight_items(resp);
        assert_eq!((items[0].title.as_str(), items[0].published), ("T & U", None));
    }
}
