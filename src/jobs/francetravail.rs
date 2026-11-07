use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use regex::Regex;
use serde::Deserialize;

use super::{
    Board, CONTRACT_CDD, CONTRACT_CDI, CONTRACT_FREELANCE, CONTRACT_INTERIM, Offer, Point, Query, parse_loose_time,
    relevance,
};
use crate::httpx::{Client, Response, decode_json, path_escape, query_string, redact};

pub const FT_TOKEN_URL: &str = "https://entreprise.francetravail.fr/connexion/oauth2/access_token?realm=%2Fpartenaire";
pub const FT_SEARCH_URL: &str = "https://api.francetravail.io/partenaire/offresdemploi/v2/offres/search";
const PUBLIC_URL: &str = "https://candidat.francetravail.fr/offres/recherche/detail/";
const SCOPE: &str = "api_offresdemploiv2 o2dsoffre";
const PAGE_SIZE: usize = 150;
/// The API refuses a `range` starting after 1000.
const MAX_START: usize = 1000;
const LOCAL_PAGES: usize = 3;
const TOKEN_MARGIN: SignedDuration = SignedDuration::from_mins(1);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);

/// "Offres d'emploi v2" API, authenticated with OAuth2 client credentials.
pub struct FranceTravail {
    http: Client,
    client_id: String,
    secret: String,
    token_url: String,
    search_url: String,
    token: Mutex<Option<(String, Timestamp)>>,
}

impl FranceTravail {
    pub fn new(http: Client, client_id: &str, secret: &str, token_url: &str, search_url: &str) -> Self {
        FranceTravail {
            http,
            client_id: client_id.to_string(),
            secret: secret.to_string(),
            token_url: token_url.to_string(),
            search_url: search_url.to_string(),
            token: Mutex::new(None),
        }
    }

    fn access_token(&self) -> Result<String, String> {
        let mut cached = self.token.lock().map_err(|_| "token cache poisoned".to_string())?;
        let now = Timestamp::now();
        if let Some((token, _)) = cached.as_ref().filter(|(_, expires)| now < *expires) {
            return Ok(token.clone());
        }
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.secret.as_str()),
            ("scope", SCOPE),
        ];
        let response = self.http.send(&self.token_url, &[], Some(&form)).map_err(|e| format!("token: {e}"))?;
        if !(200..300).contains(&response.status) {
            return Err(format!("token: POST {}: HTTP {}", redact(&self.token_url), response.status));
        }
        #[derive(Deserialize)]
        struct Token {
            #[serde(default)]
            access_token: String,
            #[serde(default)]
            expires_in: i64,
        }
        let token: Token =
            serde_json::from_slice(&response.body).map_err(|_| "token: unexpected response".to_string())?;
        if token.access_token.is_empty() {
            return Err("token: unexpected response".to_string());
        }
        let expires = now + SignedDuration::from_secs(token.expires_in) - TOKEN_MARGIN;
        *cached = Some((token.access_token.clone(), expires));
        Ok(token.access_token)
    }

    /// One retry on HTTP 429, after Retry-After capped at 5 seconds.
    fn get(&self, url: &str) -> Result<Response, String> {
        for attempt in 0.. {
            let token = self.access_token()?;
            let bearer = format!("Bearer {token}");
            let response = self
                .http
                .send(url, &[("Authorization", bearer.as_str()), ("Accept", "application/json")], None)
                .map_err(|e| e.to_string())?;
            if response.status == 429 && attempt == 0 {
                let wait = response.header("Retry-After").and_then(|s| s.trim().parse().ok()).map(Duration::from_secs);
                thread::sleep(wait.map_or(MAX_RETRY_AFTER, |w| w.min(MAX_RETRY_AFTER)));
                continue;
            }
            if !(200..300).contains(&response.status) {
                let snippet =
                    String::from_utf8_lossy(&response.body[..response.body.len().min(200)]).trim().to_string();
                return Err(format!("GET {}: HTTP {} {snippet}", redact(url), response.status));
            }
            return Ok(response);
        }
        unreachable!("the retry loop always returns")
    }

    fn search_pages(&self, params: &[(&str, String)], pages: usize) -> Result<Vec<Offer>, String> {
        let mut offers = Vec::new();
        for page in 0..pages {
            let start = page * PAGE_SIZE;
            if start > MAX_START {
                break;
            }
            let range = format!("{}-{}", start, start + PAGE_SIZE - 1);
            let mut pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            pairs.push(("range", &range));
            let url = format!("{}?{}", self.search_url, query_string(&pairs));
            let response = match self.get(&url) {
                Ok(r) => r,
                Err(_) if !offers.is_empty() => return Ok(offers),
                Err(e) => return Err(e),
            };
            let batch = parse_results(&url, &response.body)?;
            let full_page = batch.len() >= PAGE_SIZE;
            offers.extend(batch);
            match response.header("Content-Range").and_then(parse_content_range) {
                Some((last, total)) if full_page && last + 1 < total => {}
                _ => break,
            }
        }
        Ok(offers)
    }
}

impl Board for FranceTravail {
    fn name(&self) -> &'static str {
        "France Travail"
    }

    /// Pulls the whole IT domain around the origin without `motsCles` (multi-keyword semantics are undocumented); ranking filters.
    fn search(&self, q: &Query) -> Result<Vec<Offer>, String> {
        let mut params = vec![
            ("commune", q.insee.clone()),
            ("distance", q.radius_km.to_string()),
            ("publieeDepuis", "31".to_string()),
            ("sort", "1".to_string()),
        ];
        if !q.domain.is_empty() {
            params.push(("grandDomaine", q.domain.clone()));
        }
        self.search_pages(&params, LOCAL_PAGES)
    }

    fn sample(&self, q: &Query, n: usize) -> Result<Vec<Offer>, String> {
        let mut params = vec![("publieeDepuis", "7".to_string()), ("sort", "1".to_string())];
        if !q.domain.is_empty() {
            params.push(("grandDomaine", q.domain.clone()));
        }
        let offers = self.search_pages(&params, n.div_ceil(PAGE_SIZE))?;
        Ok(offers.into_iter().filter(|o| relevance(&o.title) == 1.0).collect())
    }
}

static CONTENT_RANGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)-(\d+)/(\d+)").expect("valid regex"));

/// Reads "offres 0-149/523" into (last 149, total 523).
pub fn parse_content_range(header: &str) -> Option<(usize, usize)> {
    let m = CONTENT_RANGE.captures(header)?;
    Some((m[2].parse().ok()?, m[3].parse().ok()?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Results {
    resultats: Vec<FtOffer>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct FtOffer {
    id: String,
    intitule: String,
    description: String,
    date_creation: String,
    lieu_travail: Place,
    entreprise: Named,
    type_contrat: String,
    salaire: Labelled,
    competences: Vec<Labelled>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Place {
    libelle: String,
    latitude: Option<serde_json::Value>,
    longitude: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Named {
    nom: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Labelled {
    libelle: String,
}

/// Coordinates arrive as JSON numbers or as strings depending on the offer.
fn coordinate(v: &Option<serde_json::Value>) -> Option<f64> {
    match v.as_ref()? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn parse_results(url: &str, body: &[u8]) -> Result<Vec<Offer>, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let results: Results = decode_json(url, body).map_err(|e| format!("france travail: {e}"))?;
    Ok(results
        .resultats
        .into_iter()
        .map(|r| {
            let coords = coordinate(&r.lieu_travail.latitude)
                .zip(coordinate(&r.lieu_travail.longitude))
                .map(|(lat, lon)| Point { lat, lon });
            Offer {
                url: format!("{PUBLIC_URL}{}", path_escape(&r.id)),
                id: r.id,
                source: "France Travail".to_string(),
                title: r.intitule.trim().to_string(),
                company: r.entreprise.nom.unwrap_or_default(),
                location: r.lieu_travail.libelle,
                contract: contract(&r.type_contrat),
                salary: compact_salary(&r.salaire.libelle),
                description: r.description,
                skills: r.competences.into_iter().map(|c| c.libelle).collect(),
                coords,
                posted: parse_loose_time(&r.date_creation),
                ..Offer::default()
            }
        })
        .collect())
}

fn contract(code: &str) -> String {
    match code.to_uppercase().as_str() {
        "CDI" => CONTRACT_CDI.to_string(),
        "CDD" => CONTRACT_CDD.to_string(),
        "MIS" => CONTRACT_INTERIM.to_string(),
        "LIB" => CONTRACT_FREELANCE.to_string(),
        _ => code.to_string(),
    }
}

static SALARY_AMOUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+(?:[.,]\d+)?)\s*Euros").expect("valid regex"));

/// Turns "Annuel de 40000.00 Euros à 50000.00 Euros sur 12 mois" into "40k–50k €/an".
pub fn compact_salary(label: &str) -> String {
    let label = label.trim();
    let period = if label.starts_with("Annuel") {
        "/an"
    } else if label.starts_with("Mensuel") {
        "/mois"
    } else if label.starts_with("Horaire") {
        "/h"
    } else {
        ""
    };
    let amounts: Vec<String> = SALARY_AMOUNT
        .captures_iter(label)
        .take(2)
        .filter_map(|m| m[1].replace(',', ".").parse::<f64>().ok())
        .map(|v| if v >= 1000.0 { format!("{}k", v / 1000.0) } else { format!("{v}") })
        .collect();
    if period.is_empty() || amounts.is_empty() {
        return if label.chars().count() > 40 {
            format!("{}…", label.chars().take(39).collect::<String>())
        } else {
            label.to_string()
        };
    }
    format!("{} €{period}", amounts.join("–"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::httpx::testserver::{Request, Response as Reply, TestServer};

    const PAGE: &str = r#"{"resultats":[
 {"id":"214PMGT","intitule":"Développeur PHP Laravel (H/F)","description":"Stack Laravel, React","dateCreation":"2026-10-03T08:15:00.000Z",
  "lieuTravail":{"libelle":"33 - LIBOURNE","latitude":44.9125,"longitude":-0.2328,"codePostal":"33500"},
  "entreprise":{"nom":"Acme"},"typeContrat":"CDI","salaire":{"libelle":"Annuel de 40000.00 Euros à 50000.00 Euros sur 12 mois"},
  "competences":[{"libelle":"Concevoir une API REST"}]},
 {"id":"X2","intitule":"Technicien support","lieuTravail":{"libelle":"24 - BERGERAC","latitude":"44.85","longitude":"0.49"},"typeContrat":"MIS"},
 {"id":"X3","intitule":"Développeur Go","lieuTravail":{"libelle":"30 - Inconnu"},"typeContrat":"LIB","dateCreation":"garbage","entreprise":{}}
]}"#;

    fn query_param<'a>(req: &'a Request, key: &str) -> Option<&'a str> {
        req.path.split_once('?')?.1.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}=")))
    }

    /// Serves /token (checking the client credentials form) and /search behind the bearer token.
    fn board(
        search: impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ) -> (FranceTravail, Arc<AtomicUsize>, TestServer) {
        let tokens = Arc::new(AtomicUsize::new(0));
        let counter = tokens.clone();
        let srv = TestServer::start(move |req| {
            if req.path.starts_with("/token") {
                counter.fetch_add(1, Ordering::SeqCst);
                let form = String::from_utf8_lossy(&req.body);
                if !form.contains("grant_type=client_credentials")
                    || !form.contains("client_secret=s&")
                    || !form.contains("scope=api_offresdemploiv2+o2dsoffre")
                {
                    return Reply::status(400, r#"{"error":"invalid_client"}"#);
                }
                return Reply::ok(r#"{"access_token":"tok","expires_in":1499,"token_type":"Bearer"}"#);
            }
            if req.header("Authorization") != Some("Bearer tok") {
                return Reply::status(401, "unauthorized");
            }
            search(req)
        });
        let ft = FranceTravail::new(Client::new("test", None), "id", "s", &srv.url("/token"), &srv.url("/search"));
        (ft, tokens, srv)
    }

    fn local() -> Query {
        Query { insee: "33063".into(), radius_km: 60, domain: "M18".into(), ..Query::default() }
    }

    #[test]
    fn search_maps_offers_and_caches_the_token() {
        let (ft, tokens, _srv) = board(|req| {
            assert_eq!(query_param(req, "commune"), Some("33063"));
            assert_eq!(query_param(req, "distance"), Some("60"));
            assert_eq!(query_param(req, "grandDomaine"), Some("M18"));
            assert_eq!(query_param(req, "range"), Some("0-149"));
            assert_eq!(query_param(req, "motsCles"), None);
            Reply {
                status: 200,
                headers: vec![("Content-Range".into(), "offres 0-2/3".into())],
                body: PAGE.as_bytes().to_vec(),
            }
        });
        let offers = ft.search(&local()).unwrap();
        assert_eq!(offers.len(), 3);
        let first = &offers[0];
        assert_eq!(first.url, "https://candidat.francetravail.fr/offres/recherche/detail/214PMGT");
        assert_eq!(
            (first.contract.as_str(), first.salary.as_str(), first.company.as_str()),
            (CONTRACT_CDI, "40k–50k €/an", "Acme")
        );
        assert_eq!(first.skills, ["Concevoir une API REST"]);
        assert_eq!(first.coords, Some(Point { lat: 44.9125, lon: -0.2328 }));
        assert_eq!(first.posted, Some("2026-10-03T08:15:00Z".parse().unwrap()));
        assert_eq!((offers[1].coords.map(|p| p.lat), offers[1].contract.as_str()), (Some(44.85), CONTRACT_INTERIM));
        assert_eq!((offers[2].coords, offers[2].posted, offers[2].contract.as_str()), (None, None, CONTRACT_FREELANCE));

        ft.search(&local()).unwrap();
        assert_eq!(tokens.load(Ordering::SeqCst), 1, "the token must be cached");
    }

    #[test]
    fn paginates_until_the_announced_total() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let (ft, _, _srv) = board(move |req| {
            seen.fetch_add(1, Ordering::SeqCst);
            let (start, end) = query_param(req, "range").and_then(|r| r.split_once('-')).unwrap();
            let (start, end): (usize, usize) = (start.parse().unwrap(), end.parse::<usize>().unwrap().min(199));
            let items: Vec<String> =
                (start..=end).map(|i| format!(r#"{{"id":"{i}","intitule":"Développeur {i}"}}"#)).collect();
            Reply {
                status: 206,
                headers: vec![("Content-Range".into(), format!("offres {start}-{end}/200"))],
                body: format!(r#"{{"resultats":[{}]}}"#, items.join(",")).into_bytes(),
            }
        });
        assert_eq!(ft.search(&local()).unwrap().len(), 200);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_content_after_one_rate_limit_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let (ft, _, _srv) = board(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                return Reply {
                    status: 429,
                    headers: vec![("Retry-After".into(), "0".into())],
                    body: b"slow down".to_vec(),
                };
            }
            Reply { status: 204, headers: vec![], body: vec![] }
        });
        assert!(ft.search(&local()).unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn bad_credentials_fail_on_the_token() {
        let (_, _, srv) = board(|_| Reply::ok("{}"));
        let ft = FranceTravail::new(Client::new("test", None), "id", "wrong", &srv.url("/token"), &srv.url("/search"));
        let err = ft.search(&local()).unwrap_err();
        assert!(err.starts_with("token: ") && err.contains("400"), "{err}");
    }

    #[test]
    fn national_sample_keeps_developer_titles_without_geo_filter() {
        let (ft, _, _srv) = board(|req| {
            assert_eq!(query_param(req, "commune"), None);
            assert_eq!(query_param(req, "publieeDepuis"), Some("7"));
            Reply::ok(PAGE)
        });
        let offers = ft.sample(&Query { domain: "M18".into(), ..Query::default() }, 150).unwrap();
        assert_eq!(offers.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["214PMGT", "X3"]);
    }

    #[test]
    fn content_range() {
        assert_eq!(parse_content_range("offres 0-149/523"), Some((149, 523)));
        assert_eq!(parse_content_range(""), None);
    }

    #[test]
    fn salaries() {
        for (input, want) in [
            ("Annuel de 40000.00 Euros à 50000.00 Euros sur 12 mois", "40k–50k €/an"),
            ("Annuel de 38000.00 Euros sur 12 mois", "38k €/an"),
            ("Mensuel de 2500.00 Euros sur 12 mois", "2.5k €/mois"),
            ("Horaire de 11.88 Euros", "11.88 €/h"),
            ("Selon profil", "Selon profil"),
            ("", ""),
            ("Rémunération selon expérience et profil du candidat retenu", "Rémunération selon expérience et profil…"),
        ] {
            assert_eq!(compact_salary(input), want, "{input:?}");
        }
    }
}
