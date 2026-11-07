//! Lenient RSS 2.0 / RSS 1.0 / Atom reader keeping unknown item elements (`<free>true</free>`), which feed crates drop.

use std::collections::HashMap;

use jiff::Timestamp;
use jiff::fmt::rfc2822::DateTimeParser;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub title: String,
    pub link: String,
    pub description: String,
    pub published: Option<Timestamp>,
    pub updated: Option<Timestamp>,
    pub categories: Vec<String>,
    /// Unprefixed RSS item children that are not standard fields, by element name.
    pub custom: HashMap<String, String>,
}

impl Entry {
    pub fn time(&self) -> Option<Timestamp> {
        self.published.or(self.updated)
    }
}

const RSS_FIELDS: [&str; 10] =
    ["title", "link", "description", "pubDate", "category", "guid", "author", "comments", "enclosure", "source"];

struct Field {
    name: String,
    raw: String,
}

pub fn parse(xml: &str) -> Result<Vec<Entry>, String> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    config.check_end_names = false;
    config.allow_unmatched_ends = true;
    config.allow_dangling_amp = true;

    let mut entries = Vec::new();
    let mut seen_root = false;
    let mut depth = 0usize;
    let mut entry: Option<(Entry, usize, bool)> = None;
    let mut field: Option<Field> = None;
    loop {
        let event =
            reader.read_event().map_err(|e| format!("invalid feed at byte {}: {e}", reader.error_position()))?;
        match event {
            Event::Start(e) => {
                depth += 1;
                let name = e.name().as_ref().to_string();
                if !seen_root {
                    seen_root = true;
                    if !matches!(local(&name), "rss" | "RDF" | "feed") {
                        return Err(format!("not a feed: root element <{name}>"));
                    }
                }
                match (&mut entry, &mut field) {
                    (None, _) if matches!(name.as_str(), "item" | "entry") => {
                        entry = Some((Entry::default(), depth, name == "entry"));
                    }
                    (Some((current, entry_depth, atom)), None) if depth == *entry_depth + 1 => {
                        attribute_field(current, &e, *atom);
                        field = Some(Field { name, raw: String::new() });
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if let Some((current, entry_depth, atom)) = &mut entry {
                    if field.is_none() && depth == *entry_depth {
                        attribute_field(current, &e, *atom);
                    }
                }
            }
            Event::Text(t) => push_raw(&mut field, &t.into_inner()),
            Event::GeneralRef(r) => push_raw(&mut field, &format!("&{};", r.into_inner())),
            Event::CData(c) => push_raw(&mut field, &escape(&c.into_inner())),
            Event::End(_) => {
                if let Some((current, entry_depth, atom)) = &mut entry {
                    if depth == *entry_depth + 1 {
                        if let Some(done) = field.take() {
                            assign(current, done, *atom);
                        }
                    } else if depth == *entry_depth {
                        if let Some((done, _, _)) = entry.take() {
                            entries.push(done);
                        }
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_root {
        return Err("empty document".to_string());
    }
    Ok(entries)
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn push_raw(field: &mut Option<Field>, text: &str) {
    if let Some(f) = field {
        f.raw.push_str(text);
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// XML entities plus the HTML ones feeds use without declaring them (`&nbsp;`).
fn text(raw: &str) -> String {
    htmlize::unescape(raw).trim().to_string()
}

fn attribute(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key).map(|a| htmlize::unescape(a.value.as_ref()).into_owned())
}

/// Atom keeps links and categories in attributes, on empty or non-empty elements.
fn attribute_field(entry: &mut Entry, e: &BytesStart<'_>, atom: bool) {
    if !atom {
        return;
    }
    match e.name().as_ref() {
        "link" => {
            let rel = attribute(e, "rel").unwrap_or_else(|| "alternate".to_string());
            if let Some(href) = attribute(e, "href") {
                if rel == "alternate" || entry.link.is_empty() {
                    entry.link = href;
                }
            }
        }
        "category" => entry.categories.extend(attribute(e, "term")),
        _ => {}
    }
}

fn assign(entry: &mut Entry, field: Field, atom: bool) {
    let value = text(&field.raw);
    let first = |slot: &mut String| {
        if slot.is_empty() {
            *slot = value.clone();
        }
    };
    match (field.name.as_str(), atom) {
        ("title", _) => first(&mut entry.title),
        ("link", false) => first(&mut entry.link),
        ("description", false) | ("summary", true) => first(&mut entry.description),
        ("pubDate", false) | ("published" | "issued", true) => entry.published = entry.published.or(parse_date(&value)),
        ("dc:date", _) => entry.published = entry.published.or(parse_date(&value)),
        ("updated" | "modified", true) => entry.updated = entry.updated.or(parse_date(&value)),
        ("category" | "dc:subject", false) if !value.is_empty() => entry.categories.push(value),
        (name, false) if !name.contains(':') && !RSS_FIELDS.contains(&name) => {
            entry.custom.entry(name.to_string()).or_insert(value);
        }
        _ => {}
    }
}

const LOOSE_FORMATS: [&str; 5] = [
    "%a, %d %b %Y %H:%M %z",
    "%d %b %Y %H:%M:%S %z",
    "%Y-%m-%d %H:%M:%S %z",
    "%Y-%m-%dT%H:%M:%S%z",
    "%Y-%m-%d %H:%M:%S%.f %z",
];

/// RFC 2822 (RSS), RFC 3339 (Atom), then common deviations; a missing offset means UTC.
pub fn parse_date(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    DateTimeParser::new()
        .relaxed_weekday(true)
        .parse_timestamp(s)
        .ok()
        .or_else(|| s.parse::<Timestamp>().ok())
        .or_else(|| LOOSE_FORMATS.iter().find_map(|f| Timestamp::strptime(f, s).ok()))
        .or_else(|| {
            let civil = s.parse::<jiff::civil::DateTime>().ok()?;
            civil.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Option<Timestamp> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn rss_items_with_entities_cdata_categories_and_custom_fields() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel><title>Channel</title><link>https://chan</link>
<item><title>Open article &amp; more</title><link>https://ex/1?a=1&amp;b=2</link>
<pubDate>Mon, 05 Oct 2026 08:00:00 +0000</pubDate><description><![CDATA[<p>Hello &amp; welcome</p>]]></description>
<category>Tech</category><dc:subject>AI</dc:subject><free>true</free><guid>x</guid></item>
<item><title><![CDATA[Fuite d&#8217;un &quot;secret&quot;]]></title><dc:date>2026-10-05T09:00:00+02:00</dc:date><free>false</free></item>
</channel></rss>"#;
        let entries = parse(xml).unwrap();
        assert_eq!(entries.len(), 2);
        let first = &entries[0];
        assert_eq!(first.title, "Open article & more");
        assert_eq!(first.link, "https://ex/1?a=1&b=2");
        assert_eq!(first.description, "<p>Hello &amp; welcome</p>");
        assert_eq!(first.published, ts("2026-10-05T08:00:00Z"));
        assert_eq!(first.categories, ["Tech", "AI"]);
        assert_eq!(first.custom.get("free").map(String::as_str), Some("true"));
        assert!(!first.custom.contains_key("guid"));
        assert_eq!(entries[1].title, "Fuite d&#8217;un &quot;secret&quot;");
        assert_eq!(entries[1].time(), ts("2026-10-05T07:00:00Z"));
    }

    #[test]
    fn atom_entries_use_alternate_links_term_categories_and_summary() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>The Verge</title><link href="https://site/" rel="alternate"/>
<entry><title type="html">Agents everywhere</title>
<link rel="replies" href="https://site/comments"/><link rel="alternate" type="text/html" href="https://site/a"/>
<category term="AI"/><category term="Tech"></category>
<summary type="html">&lt;p&gt;Short&lt;/p&gt;</summary><content type="xhtml"><div>Long <b>body</b></div></content>
<updated>2026-10-05T10:15:00-04:00</updated></entry>
<entry><title>Only updated</title><link href="https://site/b"/><published>2026-10-04T08:00:00Z</published><updated>2026-10-05T08:00:00Z</updated></entry>
</feed>"#;
        let entries = parse(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].link, "https://site/a");
        assert_eq!(entries[0].categories, ["AI", "Tech"]);
        assert_eq!(entries[0].description, "<p>Short</p>");
        assert_eq!((entries[0].published, entries[0].time()), (None, ts("2026-10-05T14:15:00Z")));
        assert!(entries[0].custom.is_empty());
        assert_eq!(entries[1].link, "https://site/b");
        assert_eq!(entries[1].time(), ts("2026-10-04T08:00:00Z"));
    }

    #[test]
    fn rss1_rdf_items_are_siblings_of_the_channel() {
        let xml = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel><title>c</title></channel>
<item rdf:about="https://r/1"><title>RDF story</title><link>https://r/1</link><dc:date>2026-10-05T06:00:00Z</dc:date></item>
</rdf:RDF>"#;
        let entries = parse(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].title.as_str(), entries[0].link.as_str()), ("RDF story", "https://r/1"));
        assert_eq!(entries[0].published, ts("2026-10-05T06:00:00Z"));
    }

    #[test]
    fn undeclared_html_entities_and_sloppy_markup_are_tolerated() {
        let xml = "<rss><channel><item><title>Caf&eacute;&nbsp;cr\u{e8}me &amp R&D</title><link>https://x</link></item></channel></rss>";
        let entries = parse(xml).unwrap();
        assert_eq!(entries[0].title, "Café\u{a0}crème & R&D");
    }

    #[test]
    fn non_feeds_are_rejected() {
        assert!(parse("<html><body>blocked</body></html>").unwrap_err().contains("not a feed"));
        assert!(parse("").is_err());
        assert!(parse("{\"json\": true}").is_err());
    }

    #[test]
    fn dates_in_the_wild() {
        for (input, want) in [
            ("Mon, 05 Oct 2026 10:00:00 GMT", "2026-10-05T10:00:00Z"),
            ("Mon, 5 Oct 2026 10:00:00 +0200", "2026-10-05T08:00:00Z"),
            ("Mon, 05 Oct 2026 10:00:00 EDT", "2026-10-05T14:00:00Z"),
            ("2026-10-05T10:00:00.000+02:00", "2026-10-05T08:00:00Z"),
            ("2026-10-05T10:00:00Z", "2026-10-05T10:00:00Z"),
            ("Mon, 05 Oct 2026 10:00 +0000", "2026-10-05T10:00:00Z"),
            ("2026-10-05 10:00:00 +0200", "2026-10-05T08:00:00Z"),
            ("2026-10-05T10:00:00", "2026-10-05T10:00:00Z"),
            ("Tue, 05 Oct 2026 10:00:00 +0000", "2026-10-05T10:00:00Z"),
        ] {
            assert_eq!(parse_date(input), ts(want), "{input}");
        }
        assert_eq!(parse_date("yesterday"), None);
        assert_eq!(parse_date(""), None);
    }
}
