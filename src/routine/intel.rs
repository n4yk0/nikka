use std::sync::Arc;
use std::thread::{self, JoinHandle};

use jiff::Timestamp;

use super::{Level, Routine, Section};
use crate::jobs::{self, Adzuna, Board, FranceTravail, Market, Offer, Osrm, Point, Query, RankOptions};
use crate::news::{self, Kev, Launch, Report};
use crate::skills::{self, HiringCorpora};
use crate::ui::{ago, amber, bold, countdown, cyan, muted, neon, pad_right, red, text, title};

type Fetched<T> = JoinHandle<Result<(Vec<T>, bool), String>>;

const NATIONAL_SAMPLE_SIZE: usize = 150;

/// Every external endpoint, overridable so tests can point them at a local server.
#[derive(Clone, Debug)]
pub(super) struct Endpoints {
    pub launches: String,
    pub kev: String,
    pub hn_threads: String,
    pub hn_item: String,
    pub ft_token: String,
    pub ft_search: String,
    pub adzuna: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            launches: news::LAUNCHES_URL.to_string(),
            kev: news::KEV_URL.to_string(),
            hn_threads: skills::HN_THREADS_URL.to_string(),
            hn_item: skills::HN_ITEM_URL.to_string(),
            ft_token: jobs::FT_TOKEN_URL.to_string(),
            ft_search: jobs::FT_SEARCH_URL.to_string(),
            adzuna: jobs::ADZUNA_SEARCH_URL.to_string(),
        }
    }
}

/// The market scan feeds both the skills and the jobs sections, so it is joined once and kept.
#[derive(Default)]
pub(super) enum Shared<T> {
    #[default]
    Absent,
    Pending(JoinHandle<T>),
    Ready(T),
}

/// Network fetches started together at the beginning of the run, so the briefing
/// is usually ready by the time patching and purging are done.
#[derive(Default)]
pub(super) struct Intel {
    news: Vec<JoinHandle<Report>>,
    launches: Option<Fetched<Launch>>,
    kev: Option<Fetched<Kev>>,
    pub(super) hn: Option<JoinHandle<Result<HiringCorpora, String>>>,
    pub(super) national: Option<JoinHandle<Result<Vec<Offer>, String>>>,
    pub(super) market: Shared<Market>,
}

impl Routine {
    pub(super) fn boards(&self) -> Vec<Arc<dyn Board>> {
        let mut boards: Vec<Arc<dyn Board>> = Vec::new();
        let (ft, az, e) = (&self.cfg.france_travail, &self.cfg.adzuna, &self.endpoints);
        if self.cfg.has_france_travail() {
            boards.push(Arc::new(FranceTravail::new(
                self.http.clone(),
                &ft.client_id,
                &ft.client_secret,
                &e.ft_token,
                &e.ft_search,
            )));
        }
        if self.cfg.has_adzuna() {
            boards.push(Arc::new(Adzuna::new(self.http.clone(), &az.app_id, &az.app_key, &e.adzuna)));
        }
        boards
    }

    fn job_query(&self) -> Query {
        let j = &self.cfg.jobs;
        Query {
            keywords: j.keywords.clone(),
            domain: j.ft_domain.clone(),
            insee: j.origin_insee.clone(),
            postal_code: j.origin_postal_code.clone(),
            origin: Point { lat: j.origin_lat, lon: j.origin_lon },
            radius_km: j.search_radius_km,
        }
    }

    pub(super) fn prefetch(&self) -> Intel {
        let mut intel = Intel::default();
        let now = (self.now)().timestamp();
        if self.enabled(Section::News) {
            self.prefetch_news(&mut intel, now);
        }
        let boards = self.boards();
        let query = self.job_query();
        if self.enabled(Section::Skills) {
            let (client, e) = (self.http.clone(), self.endpoints.clone());
            intel.hn = Some(thread::spawn(move || skills::fetch_hn(&client, &e.hn_threads, &e.hn_item)));
            if !boards.is_empty() {
                let (boards, query) = (boards.clone(), query.clone());
                intel.national = Some(thread::spawn(move || national_sample(&boards, &query)));
            }
        }
        if (self.enabled(Section::Jobs) || self.enabled(Section::Skills)) && !boards.is_empty() {
            let j = &self.cfg.jobs;
            let router = Osrm { base_url: j.routing_url.clone(), client: self.http.clone() };
            let opts = RankOptions {
                now,
                max_drive_minutes: j.max_drive_minutes,
                profile_skills: j.profile_skills.clone(),
                limit: j.top,
            };
            intel.market = Shared::Pending(thread::spawn(move || jobs::scan(&boards, &query, Some(&router), &opts)));
        }
        intel
    }

    fn prefetch_news(&self, intel: &mut Intel, now: Timestamp) {
        let per_category = self.cfg.news.per_category;
        for category in &self.cfg.news.categories {
            let (client, category) = (self.http.clone(), category.clone());
            let extra = category.extra.clone();
            intel.news.push(thread::spawn(move || news::fetch(&client, &category, per_category, now)));
            match extra.as_str() {
                news::EXTRA_LAUNCHES => {
                    let (client, url) = (self.http.clone(), self.endpoints.launches.clone());
                    intel.launches = Some(thread::spawn(move || news::fetch_launches(&client, &url)));
                }
                news::EXTRA_KEV => {
                    let (client, url, limit) = (self.http.clone(), self.endpoints.kev.clone(), self.cfg.news.kev_count);
                    intel.kev = Some(thread::spawn(move || news::fetch_kev(&client, &url, limit)));
                }
                _ => {}
            }
        }
    }

    pub(super) fn market<'a>(&self, intel: &'a mut Intel, label: &str) -> Option<&'a Market> {
        if matches!(intel.market, Shared::Pending(_)) {
            if let Shared::Pending(handle) = std::mem::take(&mut intel.market) {
                intel.market = Shared::Ready(self.wait(handle, label).unwrap_or_default());
            }
        }
        match &intel.market {
            Shared::Ready(market) => Some(market),
            _ => None,
        }
    }

    pub(super) fn wait<T>(&self, handle: JoinHandle<T>, label: &str) -> Option<T> {
        if handle.is_finished() {
            return handle.join().ok();
        }
        let spin = self.ui.spin(label);
        let result = handle.join().ok();
        spin.stop();
        result
    }

    pub(super) fn show_news(&mut self, intel: &mut Intel) {
        let now = (self.now)().timestamp();
        let channels = intel.news.len();
        let mut total = 0;
        for handle in std::mem::take(&mut intel.news) {
            if self.stop_requested() {
                return;
            }
            let report = self.wait(handle, "intercepting feeds").unwrap_or_default();
            self.ui.blank();
            let sources = report.category.sources.len();
            self.ui.line(&(bold(format!("▸ {}", report.category.title)) + &muted(format!("  {sources} sources"))));
            if report.items.is_empty() {
                self.ui.warn("no fresh stories");
            }
            for (i, item) in report.items.iter().enumerate() {
                self.print_story(i + 1, item, now);
            }
            total += report.items.len();
            for failure in &report.failures {
                self.ui.info(format!("offline: {}", self.ui.truncate(failure, self.ui.width().saturating_sub(16))));
            }
            match report.category.extra.as_str() {
                news::EXTRA_LAUNCHES => self.show_launches(intel.launches.take(), now),
                news::EXTRA_KEV => self.show_kev(intel.kev.take(), now),
                _ => {}
            }
        }
        self.note(Level::Ok, format!("intel: {total} stories across {channels} channels"));
    }

    fn print_story(&self, rank: usize, item: &news::Item, now: Timestamp) {
        let width = self.ui.width().saturating_sub(10);
        let headline = self.ui.link(&item.link, &title(self.ui.truncate(&item.title, width)));
        self.ui.line(&(neon(format!("{rank:02} ")) + &headline));
        let mut meta = format!("{} · {}", item.publisher, ago(item.published, now));
        if item.coverage > 1 {
            meta.push_str(&format!(" · {} sources", item.coverage));
        }
        self.ui.line(&format!("   {}", muted(meta)));
    }

    fn show_launches(&self, handle: Option<Fetched<Launch>>, now: Timestamp) {
        let Some(handle) = handle else { return };
        let (launches, stale) = match self.wait(handle, "tracking launch manifest") {
            Some(Ok(fetched)) => fetched,
            Some(Err(e)) => return self.ui.info(format!("launch manifest offline: {e}")),
            None => return self.ui.info("launch manifest offline: fetch crashed"),
        };
        self.ui.line(&(cyan("  ⟁ next SpaceX launches") + &stale_mark(stale)));
        for launch in &launches {
            let status = if launch.status == "Go" { neon("GO") } else { muted(&launch.status) };
            let name = self.ui.truncate(&launch.name, self.ui.width().saturating_sub(50));
            self.ui.line(&format!("    {}{}  {status}", neon(pad_right(&launch_window(launch, now), 18)), text(name)));
            if !launch.location.is_empty() {
                self.ui.line(&format!("    {}{}", " ".repeat(18), muted(&launch.location)));
            }
        }
    }

    fn show_kev(&self, handle: Option<Fetched<Kev>>, now: Timestamp) {
        let Some(handle) = handle else { return };
        let (entries, stale) = match self.wait(handle, "pulling CISA KEV catalog") {
            Some(Ok(fetched)) => fetched,
            Some(Err(e)) => return self.ui.info(format!("CISA KEV offline: {e}")),
            None => return self.ui.info("CISA KEV offline: fetch crashed"),
        };
        self.ui.line(&(red("  ☠ latest actively exploited CVEs (CISA KEV)") + &stale_mark(stale)));
        for kev in &entries {
            let cve = self.ui.link(&kev.link(), &amber(pad_right(&kev.cve, 16)));
            let name = self.ui.truncate(&kev.name, self.ui.width().saturating_sub(46));
            let flag = if kev.ransomware { red(" ransomware") } else { String::new() };
            self.ui.line(&format!(
                "    {cve}{}{}{flag}",
                text(name),
                muted(format!(" · {}", ago(Some(kev.date_added), now)))
            ));
        }
    }
}

/// Exact launches count down; fuzzy ones show their no-earlier-than window.
fn launch_window(launch: &Launch, now: Timestamp) -> String {
    let Some(net) = launch.net else { return "NET TBD".to_string() };
    match launch.precision.as_str() {
        "Month" | "Quarter" | "Half Year" | "Year" => format!("NET {}", net.strftime("%b %Y")),
        "Day" | "Week" => format!("NET {}", net.strftime("%a %d %b")),
        _ => countdown(net, now),
    }
}

/// Developer offers sampled nationwide by every board; an error only when every board failed.
fn national_sample(boards: &[Arc<dyn Board>], query: &Query) -> Result<Vec<Offer>, String> {
    let mut all = Vec::new();
    let mut errors = Vec::new();
    for board in boards {
        match board.sample(query, NATIONAL_SAMPLE_SIZE) {
            Ok(offers) => all.extend(offers),
            Err(e) => errors.push(format!("{}: {e}", board.name())),
        }
    }
    if all.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(all)
}

fn stale_mark(stale: bool) -> String {
    if stale { amber("  (cached, offline)") } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Setup, harness};
    use super::super::{Options, Outcome};
    use super::*;
    use crate::config::Config;
    use crate::httpx::testserver::{Response, TestServer};
    use crate::news::{Category, Source};

    const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
<item><title>Starship lands booster on tower</title><link>https://a/1</link><pubDate>Mon, 05 Oct 2026 10:00:00 +0000</pubDate></item>
<item><title>Old story</title><link>https://a/2</link><pubDate>Mon, 01 Jun 2026 10:00:00 +0000</pubDate></item>
</channel></rss>"#;

    const LAUNCHES: &str = r#"{"results":[
        {"name":"Falcon 9 | Starlink 10-12","net":"2026-10-06T03:12:00Z","net_precision":{"name":"Second"},"status":{"abbrev":"Go"},"pad":{"location":{"name":"Cape Canaveral"}}},
        {"name":"Starship | Flight 13","net":"2026-12-01T00:00:00Z","net_precision":{"name":"Month"},"status":{"name":"To Be Determined"}}]}"#;

    const KEV: &str = r#"{"vulnerabilities":[{"cveID":"CVE-2026-0001","vulnerabilityName":"Acme VPN RCE","dateAdded":"2026-10-03","knownRansomwareCampaignUse":"Known"}]}"#;

    fn server() -> TestServer {
        TestServer::start(|req| match req.path.as_str() {
            "/feed" => Response::ok(RSS),
            "/launches" => Response::ok(LAUNCHES),
            "/kev" => Response::ok(KEV),
            _ => Response::status(500, "x"),
        })
    }

    fn news_setup(srv: &TestServer, categories: Vec<Category>) -> super::super::testkit::Harness {
        let mut cfg = Config::default();
        cfg.news.categories = categories;
        let mut h = harness(Setup { sections: vec![Section::News], opts: Options::default(), cfg, ..Setup::default() });
        h.routine.endpoints.launches = srv.url("/launches");
        h.routine.endpoints.kev = srv.url("/kev");
        h
    }

    fn source(name: &str, url: String) -> Source {
        Source { name: name.into(), url, ..Source::default() }
    }

    #[test]
    fn news_section_renders_ranked_stories_and_offline_sources() {
        let srv = server();
        let category = Category {
            key: "spacex".into(),
            title: "SPACEX".into(),
            max_age_hours: 72.0,
            sources: vec![source("Feed", srv.url("/feed")), source("Broken", srv.url("/down"))],
            ..Category::default()
        };
        let mut h = news_setup(&srv, vec![category]);
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in [
            "▸ SPACEX  2 sources",
            "01 Starship lands booster on tower",
            "Feed · 2h ago",
            "offline: Broken",
            "intel: 1 stories across 1 channels",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(!plain.contains("Old story"), "stories older than max age must be dropped");
    }

    #[test]
    fn extras_show_launch_windows_and_exploited_cves() {
        let srv = server();
        let spacex = Category {
            key: "spacex".into(),
            title: "SPACEX".into(),
            extra: news::EXTRA_LAUNCHES.into(),
            sources: vec![source("Feed", srv.url("/feed"))],
            ..Category::default()
        };
        let cyber = Category {
            key: "cyber".into(),
            title: "CYBER".into(),
            extra: news::EXTRA_KEV.into(),
            sources: vec![source("Feed", srv.url("/feed"))],
            ..Category::default()
        };
        let mut h = news_setup(&srv, vec![spacex, cyber]);
        h.routine.run();
        let plain = h.out.plain();
        for want in [
            "⟁ next SpaceX launches",
            "T-15:12:00",
            "Falcon 9 | Starlink 10-12  GO",
            "Cape Canaveral",
            "NET Dec 2026",
            "To Be Determined",
            "☠ latest actively exploited CVEs (CISA KEV)",
            "CVE-2026-0001   Acme VPN RCE · 2d ago ransomware",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
    }

    #[test]
    fn offline_extras_are_reported_without_failing() {
        let srv = server();
        let spacex = Category {
            key: "spacex".into(),
            title: "SPACEX".into(),
            extra: news::EXTRA_LAUNCHES.into(),
            sources: vec![source("Feed", srv.url("/feed"))],
            ..Category::default()
        };
        let mut h = news_setup(&srv, vec![spacex]);
        h.routine.endpoints.launches = srv.url("/nope");
        assert_eq!(h.routine.run(), Outcome::Completed);
        assert!(h.out.plain().contains("launch manifest offline: GET "), "{}", h.out.plain());
    }

    #[test]
    fn launch_windows() {
        let now: Timestamp = "2026-10-05T12:00:00Z".parse().unwrap();
        let at = |precision: &str| Launch {
            net: Some("2026-10-09T18:30:00Z".parse().unwrap()),
            precision: precision.into(),
            ..Launch::default()
        };
        assert_eq!(launch_window(&at("Hour"), now), "T-4d 06:30:00");
        assert_eq!(launch_window(&at("Day"), now), "NET Fri 09 Oct");
        assert_eq!(launch_window(&at("Quarter"), now), "NET Oct 2026");
        assert_eq!(launch_window(&Launch::default(), now), "NET TBD");
    }
}
