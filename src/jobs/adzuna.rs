use serde::Deserialize;

use super::{Board, CONTRACT_CDD, CONTRACT_CDI, Offer, Point, Query, parse_loose_time, relevance};
use crate::httpx::{Client, query_string};

pub const ADZUNA_SEARCH_URL: &str = "https://api.adzuna.com/v1/api/jobs/fr/search/";
pub const ADZUNA_ATTRIBUTION: &str = "Jobs by Adzuna";
const PAGE_SIZE: usize = 50;
/// The free tier allows 250 calls a day; one run uses at most LOCAL_PAGES + 3.
const LOCAL_PAGES: usize = 2;

pub struct Adzuna {
    http: Client,
    app_id: String,
    app_key: String,
    base_url: String,
}

impl Adzuna {
    pub fn new(http: Client, app_id: &str, app_key: &str, base_url: &str) -> Self {
        Adzuna { http, app_id: app_id.to_string(), app_key: app_key.to_string(), base_url: base_url.to_string() }
    }

    fn search_pages(&self, params: &[(&str, &str)], pages: usize) -> Result<Vec<Offer>, String> {
        let page_size = PAGE_SIZE.to_string();
        let mut pairs = params.to_vec();
        pairs.extend([
            ("app_id", self.app_id.as_str()),
            ("app_key", self.app_key.as_str()),
            ("results_per_page", page_size.as_str()),
            ("content-type", "application/json"),
        ]);
        let query = query_string(&pairs);
        let mut offers = Vec::new();
        for page in 1..=pages {
            let response: Response = match self.http.get_json(&format!("{}{page}?{query}", self.base_url), &[]) {
                Ok(r) => r,
                Err(_) if !offers.is_empty() => return Ok(offers),
                Err(e) => return Err(e.to_string()),
            };
            let full_page = response.results.len() >= PAGE_SIZE;
            offers.extend(response.results.into_iter().map(Hit::into_offer));
            if !full_page || offers.len() >= response.count {
                break;
            }
        }
        Ok(offers)
    }
}

/// `what_or` splits on spaces, so a multi-word keyword like "ingénieur logiciel" would match any "ingénieur".
fn what_or(keywords: &[String]) -> String {
    keywords.iter().map(|k| k.trim()).filter(|k| !k.is_empty() && !k.contains(' ')).collect::<Vec<_>>().join(" ")
}

impl Board for Adzuna {
    fn name(&self) -> &'static str {
        "Adzuna"
    }

    fn search(&self, q: &Query) -> Result<Vec<Offer>, String> {
        let (what, radius) = (what_or(&q.keywords), q.radius_km.to_string());
        let params = [
            ("what_or", what.as_str()),
            ("where", q.postal_code.as_str()),
            ("distance", radius.as_str()),
            ("max_days_old", "31"),
            ("sort_by", "date"),
        ];
        self.search_pages(&params, LOCAL_PAGES)
    }

    fn sample(&self, q: &Query, n: usize) -> Result<Vec<Offer>, String> {
        let what = what_or(&q.keywords);
        let params = [("what_or", what.as_str()), ("max_days_old", "14"), ("sort_by", "date")];
        let offers = self.search_pages(&params, n.div_ceil(PAGE_SIZE))?;
        Ok(offers.into_iter().filter(|o| relevance(&o.title) == 1.0).collect())
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Response {
    count: usize,
    results: Vec<Hit>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Hit {
    id: String,
    title: String,
    description: String,
    created: String,
    redirect_url: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
    company: DisplayName,
    location: DisplayName,
    salary_min: Option<f64>,
    salary_max: Option<f64>,
    salary_is_predicted: Option<serde_json::Value>,
    contract_type: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DisplayName {
    display_name: String,
}

impl Hit {
    fn into_offer(self) -> Offer {
        let predicted = matches!(&self.salary_is_predicted, Some(serde_json::Value::String(s)) if s == "1")
            || matches!(&self.salary_is_predicted, Some(serde_json::Value::Number(n)) if n.as_i64() == Some(1));
        Offer {
            id: self.id,
            source: "Adzuna".to_string(),
            title: self.title.trim().to_string(),
            company: self.company.display_name,
            location: self.location.display_name,
            contract: match self.contract_type.as_deref() {
                Some("permanent") => CONTRACT_CDI.to_string(),
                Some("contract") => CONTRACT_CDD.to_string(),
                _ => String::new(),
            },
            salary: salary(self.salary_min.unwrap_or(0.0), self.salary_max.unwrap_or(0.0), predicted),
            url: self.redirect_url,
            description: self.description,
            coords: self.latitude.zip(self.longitude).map(|(lat, lon)| Point { lat, lon }),
            posted: parse_loose_time(&self.created),
            ..Offer::default()
        }
    }
}

fn salary(low: f64, high: f64, predicted: bool) -> String {
    if low <= 0.0 && high <= 0.0 {
        return String::new();
    }
    let prefix = if predicted { "~" } else { "" };
    let k = |v: f64| format!("{}k", (v / 100.0).trunc() / 10.0);
    if low > 0.0 && high > 0.0 && low != high {
        format!("{prefix}{}–{} €/an", k(low), k(high))
    } else {
        format!("{prefix}{} €/an", k(low.max(high)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::httpx::testserver::{Response as Reply, TestServer};

    #[test]
    fn search_maps_offers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let srv = TestServer::start(move |req| {
            seen.fetch_add(1, Ordering::SeqCst);
            let (path, query) = req.path.split_once('?').unwrap();
            assert!(path.ends_with("/1"), "{path}");
            for expected in [
                "app_id=id",
                "app_key=key",
                "where=33000",
                "distance=60",
                "what_or=d%C3%A9veloppeur+devops",
                "results_per_page=50",
            ] {
                assert!(query.split('&').any(|kv| kv == expected), "{expected} missing from {query}");
            }
            Reply::ok(
                r#"{"count":2,"results":[
                {"id":"1","title":"Développeur Full Stack","description":"React Node","created":"2026-10-04T09:00:00Z","redirect_url":"https://adzuna/1",
                 "latitude":44.91,"longitude":-0.23,"company":{"display_name":"Beta"},"location":{"display_name":"Libourne, Gironde"},
                 "salary_min":42000,"salary_max":52000,"salary_is_predicted":"1","contract_type":"permanent"},
                {"id":"2","title":"Commercial","company":{"display_name":"Gamma"},"location":{"display_name":"Bergerac"}}]}"#,
            )
        });
        let az = Adzuna::new(Client::new("test", None), "id", "key", &srv.url("/search/"));
        let q = Query {
            keywords: ["développeur", "ingénieur logiciel", "devops"].map(String::from).to_vec(),
            postal_code: "33000".into(),
            radius_km: 60,
            ..Query::default()
        };
        let offers = az.search(&q).unwrap();
        assert_eq!((offers.len(), calls.load(Ordering::SeqCst)), (2, 1));
        let o = &offers[0];
        assert_eq!(
            (o.contract.as_str(), o.salary.as_str(), o.url.as_str(), o.source.as_str()),
            (CONTRACT_CDI, "~42k–52k €/an", "https://adzuna/1", "Adzuna")
        );
        assert!(o.coords.is_some());
        assert_eq!((offers[1].coords, offers[1].salary.as_str(), offers[1].contract.as_str()), (None, "", ""));
    }

    #[test]
    fn errors_never_leak_the_key() {
        let srv = TestServer::start(|_| Reply::status(401, r#"{"exception":"AUTH_FAIL"}"#));
        let az = Adzuna::new(Client::new("test", None), "id", "sup3rs3cret", &srv.url("/search/"));
        let err = az.search(&Query::default()).unwrap_err();
        assert!(err.contains("401") && !err.contains("sup3rs3cret"), "{err}");
    }

    #[test]
    fn salary_formats() {
        assert_eq!(salary(0.0, 0.0, false), "");
        assert_eq!(salary(45000.0, 45000.0, false), "45k €/an");
        assert_eq!(salary(0.0, 38500.0, true), "~38.5k €/an");
    }

    #[test]
    fn what_or_keeps_single_words() {
        let keywords = ["développeur", " devops ", "product engineer", ""].map(String::from);
        assert_eq!(what_or(&keywords), "développeur devops");
    }
}
