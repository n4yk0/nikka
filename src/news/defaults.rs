use super::{Category, EXTRA_KEV, EXTRA_LAUNCHES, KIND_GOOGLE_NEWS, KIND_HACKER_NEWS, KIND_SPACEFLIGHT, Source};

const AI_TITLE_FILTER: &str = r"\b(ai|a\.i\.|llms?|gpt[-\w.]*|chatgpt|claude|openai|anthropic|gemini|mistral|deepmind|agents?|agentic|neural|inference|transformers?|machine learning)\b";

fn source(name: &str, url: &str) -> Source {
    Source { name: name.to_string(), url: url.to_string(), ..Source::default() }
}

fn google_news(name: &str, url: &str) -> Source {
    Source { kind: KIND_GOOGLE_NEWS.to_string(), editorial: true, ..source(name, url) }
}

fn weighted(name: &str, weight: f64, url: &str) -> Source {
    Source { weight, ..source(name, url) }
}

fn category(
    key: &str,
    title: &str,
    half_life_hours: f64,
    max_age_hours: f64,
    extra: &str,
    sources: Vec<Source>,
) -> Category {
    Category {
        key: key.to_string(),
        title: title.to_string(),
        half_life_hours,
        max_age_hours,
        extra: extra.to_string(),
        sources,
    }
}

pub fn default_categories() -> Vec<Category> {
    vec![
        category(
            "ai",
            "ARTIFICIAL INTELLIGENCE",
            24.0,
            96.0,
            "",
            vec![
                source("TechCrunch AI", "https://techcrunch.com/category/artificial-intelligence/feed/"),
                source("The Verge AI", "https://www.theverge.com/rss/ai-artificial-intelligence/index.xml"),
                Source {
                    skip_category: "sponsored".to_string(),
                    ..source("MIT Tech Review", "https://www.technologyreview.com/topic/artificial-intelligence/feed")
                },
                Source {
                    weight: 0.6,
                    ..google_news(
                        "Google News",
                        "https://news.google.com/rss/search?q=artificial+intelligence+when:1d&hl=en-US&gl=US&ceid=US:en",
                    )
                },
                Source {
                    kind: KIND_HACKER_NEWS.to_string(),
                    weight: 1.1,
                    filter: AI_TITLE_FILTER.to_string(),
                    ..source("Hacker News", "https://hn.algolia.com/api/v1/search?tags=front_page&hitsPerPage=50")
                },
            ],
        ),
        category(
            "spacex",
            "SPACEX",
            48.0,
            240.0,
            EXTRA_LAUNCHES,
            vec![
                Source {
                    kind: KIND_SPACEFLIGHT.to_string(),
                    ..source(
                        "Spaceflight News",
                        "https://api.spaceflightnewsapi.net/v4/articles/?search=SpaceX&ordering=-published_at&limit=20",
                    )
                },
                google_news(
                    "Google News",
                    "https://news.google.com/rss/search?q=SpaceX+when:7d&hl=en-US&gl=US&ceid=US:en",
                ),
            ],
        ),
        category(
            "cyber",
            "CYBERSECURITY",
            36.0,
            336.0,
            EXTRA_KEV,
            vec![
                weighted("CERT-FR alertes", 1.5, "https://www.cert.ssi.gouv.fr/alerte/feed/"),
                source("The Hacker News", "https://feeds.feedburner.com/TheHackersNews"),
                source("BleepingComputer", "https://www.bleepingcomputer.com/feed/"),
                weighted("Krebs on Security", 1.2, "https://krebsonsecurity.com/feed/"),
            ],
        ),
        category(
            "france",
            "FRANCE",
            18.0,
            72.0,
            "",
            // National sections only: front pages also carry world news. Both Le Monde sections share
            // a name so a story they both run counts as one feed of coverage.
            vec![
                source("Le Monde", "https://www.lemonde.fr/politique/rss_full.xml"),
                source("Le Monde", "https://www.lemonde.fr/societe/rss_full.xml"),
                source("franceinfo", "https://www.franceinfo.fr/france.rss"),
                google_news(
                    "Google News FR",
                    "https://news.google.com/rss/headlines/section/topic/NATION?hl=fr&gl=FR&ceid=FR:fr",
                ),
            ],
        ),
        category(
            "switzerland",
            "SUISSE",
            18.0,
            72.0,
            "",
            vec![
                source("RTS Info", "https://www.rts.ch/info/suisse/?format=rss/news"),
                Source { require: "free=true".to_string(), ..source("Le Temps", "https://www.letemps.ch/suisse.rss") },
                source("24 heures", "https://partner-feeds.publishing.tamedia.ch/rss/24heures/suisse"),
                google_news(
                    "Google News CH",
                    "https://news.google.com/rss/headlines/section/topic/NATION?hl=fr&gl=CH&ceid=CH:fr",
                ),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_category_is_complete() {
        let categories = default_categories();
        let keys: Vec<&str> = categories.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["ai", "spacex", "cyber", "france", "switzerland"]);
        for c in &categories {
            assert!(
                !c.sources.is_empty() && c.half_life_hours > 0.0 && c.max_age_hours > c.half_life_hours,
                "{}",
                c.key
            );
            assert!(c.sources.iter().all(|s| s.url.starts_with("https://")), "{}", c.key);
        }
    }

    #[test]
    fn ai_filter_compiles_and_matches_whole_words() {
        let re = regex::RegexBuilder::new(AI_TITLE_FILTER).case_insensitive(true).build().unwrap();
        assert!(re.is_match("OpenAI ships GPT-6"));
        assert!(!re.is_match("Paint drying in Maine"));
    }
}
