use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Deserialize;

use crate::httpx::Client;

/// Launch Library 2 allows 15 requests per hour per IP on the free tier, hence the one-hour cache.
pub const LAUNCHES_URL: &str =
    "https://ll.thespacedevs.com/2.3.0/launches/upcoming/?lsp__id=121&hide_recent_previous=true&limit=3";
const LAUNCHES_TTL: Duration = Duration::from_secs(3600);
pub const KEV_URL: &str = "https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json";
const KEV_TTL: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    pub name: String,
    pub net: Option<Timestamp>,
    pub precision: String,
    pub status: String,
    pub rocket: String,
    pub mission: String,
    pub location: String,
    pub webcast: bool,
}

#[derive(Deserialize)]
struct LlResponse {
    #[serde(default)]
    results: Vec<LlLaunch>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlLaunch {
    name: String,
    net: Option<String>,
    net_precision: Option<Named>,
    status: LlStatus,
    pad: LlPad,
    rocket: LlRocket,
    mission: Option<LlMission>,
    webcast_live: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Named {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlStatus {
    name: String,
    abbrev: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlPad {
    location: Named,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlRocket {
    configuration: Named,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlMission {
    #[serde(rename = "type")]
    kind: String,
}

pub fn parse_launches(data: &[u8]) -> Result<Vec<Launch>, serde_json::Error> {
    let resp: LlResponse = serde_json::from_slice(data)?;
    Ok(resp
        .results
        .into_iter()
        .map(|r| Launch {
            name: r.name,
            net: r.net.and_then(|n| n.parse().ok()),
            precision: r.net_precision.map(|p| p.name).unwrap_or_default(),
            status: if r.status.abbrev.is_empty() { r.status.name } else { r.status.abbrev },
            rocket: r.rocket.configuration.name,
            mission: r.mission.map(|m| m.kind).unwrap_or_default(),
            location: r.pad.location.name,
            webcast: r.webcast_live,
        })
        .collect())
}

/// Returns the launches and whether they come from an offline (stale) cache.
pub fn fetch_launches(client: &Client, url: &str) -> Result<(Vec<Launch>, bool), String> {
    let cached = client.get_cached(url, LAUNCHES_TTL).map_err(|e| e.to_string())?;
    parse_launches(&cached.body).map(|l| (l, cached.stale)).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Kev {
    pub cve: String,
    pub vendor: String,
    pub product: String,
    pub name: String,
    pub date_added: Timestamp,
    pub due_date: Option<Timestamp>,
    pub ransomware: bool,
}

impl Kev {
    pub fn link(&self) -> String {
        format!("https://nvd.nist.gov/vuln/detail/{}", self.cve)
    }
}

#[derive(Deserialize)]
struct KevCatalog {
    #[serde(default)]
    vulnerabilities: Vec<KevEntry>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct KevEntry {
    #[serde(rename = "cveID")]
    cve_id: String,
    vendor_project: String,
    product: String,
    vulnerability_name: String,
    date_added: String,
    due_date: String,
    known_ransomware_campaign_use: String,
}

fn utc_midnight(s: &str) -> Option<Timestamp> {
    s.parse::<Date>().ok()?.to_zoned(TimeZone::UTC).ok().map(|z| z.timestamp())
}

/// Newest additions first; entries without a valid `dateAdded` are dropped. A zero limit keeps everything.
pub fn parse_kev(data: &[u8], limit: usize) -> Result<Vec<Kev>, serde_json::Error> {
    let catalog: KevCatalog = serde_json::from_slice(data)?;
    let mut entries: Vec<Kev> = catalog
        .vulnerabilities
        .into_iter()
        .filter_map(|v| {
            Some(Kev {
                date_added: utc_midnight(&v.date_added)?,
                due_date: utc_midnight(&v.due_date),
                ransomware: v.known_ransomware_campaign_use == "Known",
                cve: v.cve_id,
                vendor: v.vendor_project,
                product: v.product,
                name: v.vulnerability_name,
            })
        })
        .collect();
    entries.sort_by(|a, b| b.date_added.cmp(&a.date_added));
    if limit > 0 {
        entries.truncate(limit);
    }
    Ok(entries)
}

pub fn fetch_kev(client: &Client, url: &str, limit: usize) -> Result<(Vec<Kev>, bool), String> {
    let cached = client.get_cached(url, KEV_TTL).map_err(|e| e.to_string())?;
    parse_kev(&cached.body, limit).map(|k| (k, cached.stale)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches() {
        let data = br#"{"results":[{"name":"Falcon 9 Block 5 | Starlink Group 10-12","net":"2026-10-06T03:12:00Z",
            "net_precision":{"name":"Second"},"status":{"name":"Go for Launch","abbrev":"Go"},
            "pad":{"location":{"name":"Cape Canaveral SFS, FL, USA"}},"rocket":{"configuration":{"name":"Falcon 9"}},
            "mission":{"name":"Starlink","type":"Communications"},"webcast_live":false},
            {"name":"Starship | Flight 13","net":"2026-10-31T00:00:00Z","net_precision":null,"status":{"name":"To Be Determined","abbrev":""},"mission":null}]}"#;
        let launches = parse_launches(data).unwrap();
        assert_eq!(launches.len(), 2);
        let first = &launches[0];
        assert_eq!((first.status.as_str(), first.precision.as_str()), ("Go", "Second"));
        assert_eq!(
            (first.location.as_str(), first.rocket.as_str(), first.mission.as_str()),
            ("Cape Canaveral SFS, FL, USA", "Falcon 9", "Communications")
        );
        assert_eq!(first.net, Some("2026-10-06T03:12:00Z".parse().unwrap()));
        let second = &launches[1];
        assert_eq!(
            (second.status.as_str(), second.mission.as_str(), second.precision.as_str()),
            ("To Be Determined", "", "")
        );
        assert!(parse_launches(b"<html>").is_err());
    }

    #[test]
    fn kev_entries_newest_first() {
        let data = br#"{"vulnerabilities":[
            {"cveID":"CVE-2026-0001","vendorProject":"Acme","product":"VPN","vulnerabilityName":"Acme VPN RCE","dateAdded":"2026-09-01","dueDate":"2026-09-22","knownRansomwareCampaignUse":"Known"},
            {"cveID":"CVE-2026-0002","vendorProject":"Foo","product":"Bar","vulnerabilityName":"Foo Bar","dateAdded":"2026-10-03","dueDate":"2026-10-24","knownRansomwareCampaignUse":"Unknown"},
            {"cveID":"CVE-BAD","dateAdded":"not-a-date"}]}"#;
        let top = parse_kev(data, 1).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].cve.as_str(), top[0].ransomware), ("CVE-2026-0002", false));
        let all = parse_kev(data, 0).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[1].ransomware);
        assert_eq!(all[1].link(), "https://nvd.nist.gov/vuln/detail/CVE-2026-0001");
        assert_eq!(all[1].date_added, "2026-09-01T00:00:00Z".parse().unwrap());
        assert!(parse_kev(b"<html>", 0).is_err());
    }
}
