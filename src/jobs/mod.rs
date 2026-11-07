//! Searches job boards around an origin, keeps offers within a driving-time budget and ranks them.

mod adzuna;
mod francetravail;
mod market;
mod route;

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use jiff::Timestamp;
use regex::Regex;

pub use adzuna::{ADZUNA_ATTRIBUTION, ADZUNA_SEARCH_URL, Adzuna};
pub use francetravail::{FT_SEARCH_URL, FT_TOKEN_URL, FranceTravail, compact_salary, parse_content_range};
pub use market::{Market, scan};
pub use route::{Osrm, attach_drive_times, estimate_drive_minutes, haversine_km};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offer {
    pub id: String,
    pub source: String,
    pub title: String,
    pub company: String,
    pub location: String,
    pub contract: String,
    pub salary: String,
    pub url: String,
    pub description: String,
    pub skills: Vec<String>,
    pub coords: Option<Point>,
    pub posted: Option<Timestamp>,
    pub drive_minutes: Option<f64>,
    /// The drive time is a straight-line estimate, not a routed one.
    pub drive_estimated: bool,
    pub matched: Vec<String>,
    pub score: f64,
}

impl Offer {
    pub fn text(&self) -> String {
        format!("{}\n{}\n{}", self.title, self.description, self.skills.join(", "))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub keywords: Vec<String>,
    pub domain: String,
    pub insee: String,
    pub postal_code: String,
    pub origin: Point,
    pub radius_km: u32,
}

/// A job board: a local search around the origin and a national sample for the skills analysis.
pub trait Board: Send + Sync {
    fn name(&self) -> &'static str;
    fn search(&self, q: &Query) -> Result<Vec<Offer>, String>;
    fn sample(&self, q: &Query, n: usize) -> Result<Vec<Offer>, String>;
}

pub const CONTRACT_CDI: &str = "CDI";
pub const CONTRACT_CDD: &str = "CDD";
pub const CONTRACT_INTERIM: &str = "Intérim";
pub const CONTRACT_FREELANCE: &str = "Freelance";

const WEIGHT_RELEVANCE: f64 = 0.35;
const WEIGHT_SKILLS: f64 = 0.30;
const WEIGHT_RECENCY: f64 = 0.15;
const WEIGHT_CONTRACT: f64 = 0.10;
const WEIGHT_PROXIMITY: f64 = 0.10;

const RECENCY_HALF_LIFE_DAYS: f64 = 10.0;
const UNKNOWN_DRIVE_PENALTY: f64 = 0.3;
const SKILL_MATCH_SATURATION: usize = 4;

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid jobs pattern")
}

static STRONG_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?:^|[^a-z])(?:developpeu|developer|software|ingenieur (?:logiciel|developpement|informatique|etudes)|full[- ]?stack|back[- ]?end|front[- ]?end|devops|sre|product engineer|tech lead|lead dev|architecte (?:logiciel|applicatif|technique|cloud)|data engineer|ml engineer|ai engineer|ingenieur (?:ia|ai|data|devops|cloud))",
    )
});
static WEAK_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?:^|[^a-z])(?:informatique|logiciel|web|cloud|data|it|si|digital|numerique|integrateur|programmeu)")
});
static EXCLUDED: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?:^|[^a-z])(?:alternan[a-z]*|apprenti[a-z]*|stages?|stagiaire|internship|intern)(?:$|[^a-z])")
});
static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| regex(r"[^a-z0-9]+"));

/// 1 for a software-development title, 0.4 for a loosely IT one, 0.1 otherwise.
pub fn relevance(title: &str) -> f64 {
    let folded = crate::textutil::fold(title);
    if STRONG_TITLE.is_match(&folded) {
        1.0
    } else if WEAK_TITLE.is_match(&folded) {
        0.4
    } else {
        0.1
    }
}

fn contract_score(contract: &str) -> f64 {
    match contract {
        CONTRACT_CDI => 1.0,
        CONTRACT_FREELANCE => 0.8,
        CONTRACT_CDD | "" => 0.6,
        CONTRACT_INTERIM => 0.4,
        _ => 0.5,
    }
}

fn skill_matchers(profile: &[String]) -> BTreeMap<String, Regex> {
    profile
        .iter()
        .map(|s| crate::textutil::fold(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let re = regex(&format!("(?:^|[^a-z0-9]){}(?:$|[^a-z0-9])", regex::escape(&s)));
            (s, re)
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct RankOptions {
    pub now: Timestamp,
    pub max_drive_minutes: f64,
    pub profile_skills: Vec<String>,
    /// Zero keeps every offer.
    pub limit: usize,
}

/// Drops offers beyond the drive budget, internships and duplicates, then scores what remains.
pub fn rank(offers: &[Offer], opts: &RankOptions) -> Vec<Offer> {
    let matchers = skill_matchers(&opts.profile_skills);
    let mut seen = HashSet::new();
    let mut ranked = Vec::new();
    for offer in offers {
        let key = dedupe_key(offer);
        if seen.contains(&key) || EXCLUDED.is_match(&crate::textutil::fold(&offer.title)) {
            continue;
        }
        if offer.drive_minutes.is_some_and(|m| m > opts.max_drive_minutes) {
            continue;
        }
        seen.insert(key);

        let mut o = offer.clone();
        let folded = crate::textutil::fold(&o.text());
        o.matched = matchers.iter().filter(|(_, re)| re.is_match(&folded)).map(|(name, _)| name.clone()).collect();
        let skills = if matchers.is_empty() {
            0.0
        } else {
            (o.matched.len() as f64 / matchers.len().min(SKILL_MATCH_SATURATION) as f64).min(1.0)
        };
        let recency = o.posted.map_or(0.5, |p| {
            let days = opts.now.duration_since(p).as_secs_f64() / 86_400.0;
            0.5f64.powf(days / RECENCY_HALF_LIFE_DAYS)
        });
        let proximity = o.drive_minutes.map_or(UNKNOWN_DRIVE_PENALTY, |m| (1.0 - m / opts.max_drive_minutes).max(0.0));
        o.score = WEIGHT_RELEVANCE * relevance(&o.title)
            + WEIGHT_SKILLS * skills
            + WEIGHT_RECENCY * recency
            + WEIGHT_CONTRACT * contract_score(&o.contract)
            + WEIGHT_PROXIMITY * proximity;
        ranked.push(o);
    }
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    if opts.limit > 0 {
        ranked.truncate(opts.limit);
    }
    ranked
}

fn dedupe_key(o: &Offer) -> String {
    let title = crate::textutil::fold(&o.title).replace("(h/f)", "");
    let company = crate::textutil::fold(&o.company);
    format!("{}|{}", NON_ALNUM.replace_all(&title, ""), NON_ALNUM.replace_all(&company, ""))
}

/// Parses the dates boards send: RFC 3339, or a naive date-time / date read as UTC.
fn parse_loose_time(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    s.parse::<Timestamp>().ok().or_else(|| {
        let civil = s
            .parse::<jiff::civil::DateTime>()
            .ok()
            .or_else(|| s.parse::<jiff::civil::Date>().ok().map(|d| d.to_datetime(jiff::civil::Time::midnight())))?;
        civil.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp())
    })
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;

    pub(super) fn now() -> Timestamp {
        "2026-10-05T12:00:00Z".parse().unwrap()
    }

    fn opts() -> RankOptions {
        RankOptions {
            now: now(),
            max_drive_minutes: 45.0,
            profile_skills: ["php", "react", "kubernetes", "C++"].map(String::from).to_vec(),
            limit: 10,
        }
    }

    fn offer(title: &str, company: &str, contract: &str, drive: Option<f64>, hours_ago: i64, desc: &str) -> Offer {
        Offer {
            title: title.into(),
            company: company.into(),
            contract: contract.into(),
            drive_minutes: drive,
            posted: Some(now() - SignedDuration::from_hours(hours_ago)),
            description: desc.into(),
            ..Offer::default()
        }
    }

    #[test]
    fn rank_orders_by_relevance_and_filters() {
        let offers = [
            offer(
                "Développeur Fullstack PHP / React (H/F)",
                "Acme",
                CONTRACT_CDI,
                Some(15.0),
                24,
                "Stack PHP 8, React, Kubernetes",
            ),
            offer("Technicien support informatique", "Beta", CONTRACT_CDD, Some(20.0), 24, "helpdesk"),
            offer("Développeur fullstack PHP/React (h/f)", "ACME", CONTRACT_CDI, Some(15.0), 24, "duplicate"),
            offer("Alternance développeur web", "Gamma", CONTRACT_CDD, Some(10.0), 1, "php react"),
            offer("Software Engineer C++", "Delta", CONTRACT_CDI, Some(70.0), 1, "C++"),
            offer("Ingénieur logiciel Java", "Epsilon", CONTRACT_INTERIM, None, 20 * 24, "Java"),
        ];
        let ranked = rank(&offers, &opts());
        let titles: Vec<&str> = ranked.iter().map(|o| o.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Développeur Fullstack PHP / React (H/F)", "Ingénieur logiciel Java", "Technicien support informatique"]
        );
        assert_eq!(ranked[0].matched, ["kubernetes", "php", "react"]);
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn skills_with_symbols_match() {
        let ranked = rank(&[offer("Dev C++", "x", CONTRACT_CDI, Some(5.0), 1, "Modern C++20")], &opts());
        assert_eq!(ranked[0].matched, ["c++"]);
    }

    #[test]
    fn limit_and_empty_input() {
        assert!(rank(&[], &opts()).is_empty());
        let many: Vec<Offer> = ["Go", "Rust", "Python"]
            .iter()
            .map(|lang| offer(&format!("Développeur {lang}"), lang, CONTRACT_CDI, Some(10.0), 1, ""))
            .collect();
        assert_eq!(rank(&many, &RankOptions { limit: 2, ..opts() }).len(), 2);
    }

    #[test]
    fn contract_preference_order() {
        assert!(contract_score(CONTRACT_CDI) > contract_score(CONTRACT_FREELANCE));
        assert!(contract_score(CONTRACT_FREELANCE) > contract_score(CONTRACT_CDD));
        assert!(contract_score(CONTRACT_CDD) > contract_score(CONTRACT_INTERIM));
    }

    #[test]
    fn loose_times() {
        assert_eq!(parse_loose_time("2026-10-03T08:15:00.000Z"), Some("2026-10-03T08:15:00Z".parse().unwrap()));
        assert_eq!(parse_loose_time("2026-10-03T08:15:00"), Some("2026-10-03T08:15:00Z".parse().unwrap()));
        assert_eq!(parse_loose_time("2026-10-03"), Some("2026-10-03T00:00:00Z".parse().unwrap()));
        assert_eq!(parse_loose_time("garbage"), None);
    }
}
