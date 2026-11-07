use std::collections::HashSet;
use std::sync::LazyLock;

use jiff::{SignedDuration, Timestamp};

use super::Item;
use crate::textutil::fold;

const CLUSTER_THRESHOLD: f64 = 0.35;
const COVERAGE_BONUS: f64 = 0.4;
const EDITORIAL_DECAY: f64 = 0.08;
const MIN_CLUSTER_TOKENS: usize = 3;

#[derive(Clone, Debug)]
pub struct RankOptions {
    pub now: Timestamp,
    pub half_life: SignedDuration,
    /// Zero keeps every story regardless of age.
    pub max_age: SignedDuration,
    /// Zero keeps every cluster.
    pub limit: usize,
}

struct Candidate {
    item: Item,
    base: f64,
    tokens: HashSet<String>,
}

struct Cluster {
    candidate: Candidate,
    feeds: HashSet<String>,
}

/// Groups near-duplicate titles across feeds, then scores each story by recency,
/// source weight, editorial position and how many feeds covered it.
pub fn rank(items: Vec<Item>, opts: &RankOptions) -> Vec<Item> {
    let mut seen_links = HashSet::new();
    let mut candidates = Vec::new();
    for item in items {
        if item.title.is_empty() || (!item.link.is_empty() && seen_links.contains(&item.link)) {
            continue;
        }
        let age = item.published.map_or(opts.half_life, |p| opts.now.duration_since(p)).max(SignedDuration::ZERO);
        if !opts.max_age.is_zero() && age > opts.max_age {
            continue;
        }
        seen_links.insert(item.link.clone());
        let base = base_score(&item, age, opts.half_life);
        let tokens = tokens(&item.title);
        candidates.push(Candidate { item, base, tokens });
    }
    candidates.sort_by(|a, b| b.base.total_cmp(&a.base));

    let mut clusters: Vec<Cluster> = Vec::new();
    for c in candidates {
        match clusters.iter_mut().find(|cl| similar(&cl.candidate.tokens, &c.tokens)) {
            Some(home) => {
                home.feeds.insert(c.item.feed.clone());
            }
            None => {
                let feeds = HashSet::from([c.item.feed.clone()]);
                clusters.push(Cluster { candidate: c, feeds });
            }
        }
    }

    let mut ranked: Vec<Item> = clusters
        .into_iter()
        .map(|cl| {
            let mut item = cl.candidate.item;
            item.coverage = cl.feeds.len();
            item.score = cl.candidate.base * (1.0 + COVERAGE_BONUS * (item.coverage - 1) as f64);
            item
        })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    if opts.limit > 0 {
        ranked.truncate(opts.limit);
    }
    ranked
}

fn base_score(item: &Item, age: SignedDuration, half_life: SignedDuration) -> f64 {
    let weight = if item.weight > 0.0 { item.weight } else { 1.0 };
    let boost = if item.boost > 0.0 { item.boost } else { 1.0 };
    let recency = if half_life.is_positive() { 0.5f64.powf(age.as_secs_f64() / half_life.as_secs_f64()) } else { 1.0 };
    let position = if item.editorial { 1.0 / (1.0 + EDITORIAL_DECAY * item.position as f64) } else { 1.0 };
    weight * boost * recency * position
}

fn similar(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    let inter = a.intersection(b).count();
    if a.len() < MIN_CLUSTER_TOKENS || b.len() < MIN_CLUSTER_TOKENS {
        return !a.is_empty() && a.len() == b.len() && inter == a.len();
    }
    let union = a.len() + b.len() - inter;
    inter as f64 / union as f64 >= CLUSTER_THRESHOLD
}

static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "the and for with from that this are was were has have its into over after about new says will \
     les des une est pour dans par sur avec qui que aux ses son sont pas plus elle ils leur cette entre apres \
     face fait selon contre lors comme mais tout deux trois"
        .split_whitespace()
        .collect()
});

pub fn tokens(s: &str) -> HashSet<String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3 && !STOPWORDS.contains(t))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-10-05T12:00:00Z".parse().unwrap()
    }

    fn item(feed: &str, title: &str, age_hours: i64) -> Item {
        Item {
            feed: feed.into(),
            title: title.into(),
            link: format!("https://x/{feed}/{title}"),
            published: Some(now() - SignedDuration::from_hours(age_hours)),
            weight: 1.0,
            boost: 1.0,
            ..Item::default()
        }
    }

    fn opts(half_life_hours: i64, max_age_hours: i64, limit: usize) -> RankOptions {
        RankOptions {
            now: now(),
            half_life: SignedDuration::from_hours(half_life_hours),
            max_age: SignedDuration::from_hours(max_age_hours),
            limit,
        }
    }

    #[test]
    fn tokens_fold_accents_and_drop_stopwords() {
        let tok = tokens("Élections : la France et les réformes d'été");
        for want in ["elections", "france", "reformes", "ete"] {
            assert!(tok.contains(want), "missing {want} in {tok:?}");
        }
        for unwanted in ["les", "la", "et"] {
            assert!(!tok.contains(unwanted), "stopword {unwanted} kept");
        }
    }

    #[test]
    fn coverage_and_recency_win() {
        let items = vec![
            item("A", "Starship flight 12 lands booster on tower", 6),
            item("B", "Starship flight 12: booster lands on tower again", 7),
            item("C", "Starship flight 12 booster lands tower catch", 8),
            item("A", "Minor update to Falcon 9 manifest", 1),
            item("B", "Ancient story", 30 * 24),
        ];
        let ranked = rank(items, &opts(24, 72, 5));
        assert_eq!(ranked.len(), 2, "{ranked:?}");
        assert_eq!(ranked[0].coverage, 3);
        assert!(ranked[0].title.starts_with("Starship flight 12 lands"));
        assert_eq!(ranked[1].coverage, 1);
    }

    #[test]
    fn editorial_position_and_limit() {
        let items = [
            "Gouvernement remaniement annoncé ce soir",
            "Météo orages violents attendus",
            "Football ligue résultats complets",
        ]
        .iter()
        .enumerate()
        .map(|(i, title)| Item { editorial: true, position: i, ..item("Le Monde", title, 1) })
        .collect();
        let ranked = rank(items, &opts(18, 0, 2));
        assert_eq!(ranked.iter().map(|i| i.position).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn duplicate_links_future_and_undated_items() {
        let a = item("A", "Same link story here", 1);
        let b = Item { feed: "B".into(), ..a.clone() };
        let future = item("C", "Clock skew story from the future", -2);
        let undated = Item { published: None, ..item("D", "Undated editorial headline today", 0) };
        let ranked = rank(vec![a, b, future, undated], &opts(24, 48, 0));
        assert_eq!(ranked.len(), 3, "{ranked:?}");
        assert_eq!(ranked[0].feed, "C", "a future-dated item counts as brand new");
        assert_eq!(ranked[2].feed, "D", "an undated item ranks last");
        assert!(rank(Vec::new(), &opts(24, 0, 0)).is_empty());
    }

    #[test]
    fn short_titles_only_cluster_when_identical() {
        let mut a = HashSet::new();
        a.insert("spacex".to_string());
        let b = a.clone();
        let mut c = a.clone();
        c.insert("nasa".to_string());
        assert!(similar(&a, &b) && !similar(&a, &c) && !similar(&HashSet::new(), &HashSet::new()));
    }
}
