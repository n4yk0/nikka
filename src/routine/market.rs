use jiff::Timestamp;

use super::intel::Intel;
use super::{Level, Routine};
use crate::httpx::query_string;
use crate::jobs::{ADZUNA_ATTRIBUTION, Offer};
use crate::skills::{self, Corpus, CorpusInfo, Stat};
use crate::ui::{ago, bar, cyan, delta, muted, neon, pad_left, pad_right, percent, tag, text, title};

/// Below this, the local market is too thin to say anything about skills.
const MIN_LOCAL_CORPUS: usize = 8;

fn offers_corpus(name: &str, label: &str, offers: &[Offer]) -> Corpus {
    Corpus { name: name.to_string(), label: label.to_string(), docs: offers.iter().map(Offer::text).collect() }
}

fn short_label(c: &CorpusInfo) -> &'static str {
    match c.name.as_str() {
        "hn" => "HN",
        "fr" => "FR",
        _ => "local",
    }
}

fn trimmed_percent(f: f64) -> String {
    percent(f).trim().to_string()
}

impl Routine {
    pub(super) fn show_skills(&mut self, intel: &mut Intel) {
        let mut corpora = Vec::new();
        let mut baseline = None;
        match intel.hn.take().map(|h| self.wait(h, "parsing HN \"Who is hiring?\" threads")) {
            Some(Some(Ok(hn))) => {
                corpora.push(hn.current);
                baseline = hn.previous;
            }
            Some(Some(Err(e))) => self.ui.info(format!("HN hiring threads offline: {e}")),
            Some(None) => self.ui.info("HN hiring threads offline: fetch crashed"),
            None => {}
        }
        match intel.national.take().map(|h| self.wait(h, "sampling French job market")) {
            Some(Some(Ok(national))) if !national.is_empty() => {
                corpora.push(offers_corpus("fr", "France national", &national))
            }
            Some(Some(Err(e))) => self.ui.info(format!("national sample offline: {e}")),
            _ => {}
        }
        let max_drive = self.cfg.jobs.max_drive_minutes;
        if let Some(market) = self.market(intel, "scanning local market") {
            if market.local.len() >= MIN_LOCAL_CORPUS {
                corpora.push(offers_corpus("local", &format!("≤{max_drive:.0}min local"), &market.local));
            }
        }
        if corpora.is_empty() {
            self.ui.warn("no corpus available, skills analysis skipped");
            self.note(Level::Warn, "skills: no data source reachable".to_string());
            return;
        }

        let analysis = skills::analyze(&corpora, baseline.as_ref());
        let sources: Vec<String> = analysis.corpora.iter().map(|c| format!("{} ({} posts)", c.label, c.docs)).collect();
        self.ui.info(format!("corpus: {}", sources.join(" · ")));
        if let Some(previous) = &baseline {
            self.ui.info(format!("trend Δ: {} vs {}", corpora[0].label, previous.label));
        }
        self.ui.blank();

        let mut header = format!("   {}{}", pad_right("skill", 26), pad_right("demand", 17));
        for c in &analysis.corpora {
            header.push_str(&pad_left(short_label(c), 10));
        }
        if baseline.is_some() {
            header.push_str(&pad_left("Δ m/m", 11));
        }
        self.ui.line(&muted(header));
        let top = analysis.top(self.cfg.skills.top);
        for (i, stat) in top.iter().enumerate() {
            self.ui.line(&skill_row(i + 1, stat));
        }

        self.ui.blank();
        self.ui.line(&cyan("  ⌁ market signals"));
        for signal in &analysis.signals {
            let parts: Vec<String> = analysis
                .corpora
                .iter()
                .zip(&signal.shares)
                .map(|(c, share)| format!("{} {}", short_label(c), trimmed_percent(*share)))
                .collect();
            let mut line = format!("    {}{}", text(pad_right(signal.name, 30)), muted(parts.join(" · ")));
            if let Some(d) = signal.delta {
                line.push_str(&format!("  {}", delta(d)));
            }
            self.ui.line(&line);
        }
        if baseline.is_some() {
            let mut risers: Vec<&Stat> = analysis.skills.iter().collect();
            risers.sort_by(|a, b| b.delta.unwrap_or(0.0).total_cmp(&a.delta.unwrap_or(0.0)));
            if let (Some(up), Some(down)) = (risers.first(), risers.last()) {
                self.ui.line(&format!(
                    "    {}{}{}{}{}",
                    text(pad_right("fastest riser / faller", 30)),
                    muted(format!("{} ", up.name)),
                    delta(up.delta.unwrap_or(0.0)),
                    muted(format!(" · {} ", down.name)),
                    delta(down.delta.unwrap_or(0.0))
                ));
            }
        }
        let names: Vec<&str> = top.iter().map(|s| s.name).collect();
        self.note(Level::Ok, format!("skills: top {} → {}", top.len(), names.join(", ")));
    }

    pub(super) fn show_jobs(&mut self, intel: &mut Intel) {
        let now = (self.now)().timestamp();
        let max_drive = self.cfg.jobs.max_drive_minutes;
        let Some(market) = self.market(intel, "scanning job boards").cloned() else {
            self.ui.warn("no job board configured");
            self.jobs_setup_help();
            self.ui.blank();
            self.print_quick_links();
            self.note(Level::Warn, "jobs: no job board credentials".to_string());
            return;
        };
        for failure in &market.failures {
            self.ui.fail(self.ui.truncate(failure, self.ui.width().saturating_sub(8)));
        }
        let routing = if market.routed { "OSRM road routing" } else { "straight-line estimate (router offline)" };
        self.ui.info(format!(
            "{} offers scanned via {} · {} within {max_drive:.0} min · {routing}",
            market.considered,
            market.providers.join(" + "),
            market.local.len()
        ));
        let Some(best) = market.ranked.first() else {
            self.ui.warn("no matching offer in range today");
            self.note(Level::Warn, "jobs: no offer in range".to_string());
            return;
        };
        for (i, offer) in market.ranked.iter().enumerate() {
            self.print_offer(i + 1, offer, now);
        }
        self.ui.blank();
        if market.ranked.iter().any(|o| o.source == "Adzuna") {
            self.ui.info(ADZUNA_ATTRIBUTION);
        }
        self.print_quick_links();
        self.note(Level::Ok, format!("jobs: {} targets locked (best: {})", market.ranked.len(), best.title));
    }

    fn print_offer(&self, rank: usize, o: &Offer, now: Timestamp) {
        self.ui.blank();
        let drive = match o.drive_minutes {
            Some(minutes) => cyan(format!("⏱ {}{minutes:.0} min", if o.drive_estimated { "~" } else { "" })),
            None => muted("⏱ ?"),
        };
        let right = if o.contract.is_empty() { drive } else { format!("{} {drive}", tag(&o.contract)) };
        let title_width = self.ui.width().saturating_sub(34);
        let headline = self.ui.link(&o.url, &title(self.ui.truncate(&o.title, title_width)));
        self.ui.line(&format!("{}{}  {right}", neon(format!("{rank:02} ▸ ")), pad_right(&headline, title_width)));

        let posted = ago(o.posted, now);
        let meta: Vec<&str> =
            [o.company.as_str(), o.location.as_str(), o.salary.as_str(), posted.as_str(), o.source.as_str()]
                .into_iter()
                .filter(|v| !v.trim().is_empty())
                .collect();
        self.ui
            .line(&format!("     {}", muted(self.ui.truncate(&meta.join(" · "), self.ui.width().saturating_sub(10)))));
        if !o.matched.is_empty() {
            self.ui.line(&format!("     {}{}", neon("match "), text(o.matched.join(" · "))));
        }
    }

    fn quick_links(&self) -> [(&'static str, String); 2] {
        let j = &self.cfg.jobs;
        let france_travail = query_string(&[
            ("motsCles", "developpeur"),
            ("lieux", &j.origin_insee),
            ("rayon", "50"),
            ("tri", "0"),
            ("offresPartenaires", "true"),
        ]);
        let location = format!("{}, {}, France", j.origin_name, j.origin_region);
        let linkedin = query_string(&[
            ("keywords", "développeur"),
            ("location", &location),
            ("distance", "25"),
            ("f_TPR", "r604800"),
        ]);
        [
            ("France Travail", format!("https://candidat.francetravail.fr/offres/recherche?{france_travail}")),
            ("LinkedIn", format!("https://www.linkedin.com/jobs/search/?{linkedin}")),
        ]
    }

    fn print_quick_links(&self) {
        for (label, url) in self.quick_links() {
            let shown = if self.ui.tty() { label.to_string() } else { format!("{label} {url}") };
            self.ui.line(&(cyan("↗ ") + &self.ui.link(&url, &text(shown))));
        }
    }

    fn jobs_setup_help(&self) {
        self.ui.line(&(text("  France Travail (free): register an app on ") + &cyan("https://francetravail.io")));
        self.ui.line(&text("  subscribe it to \"API Offres d'emploi v2\", then export:"));
        self.ui.line(&muted("    export FRANCE_TRAVAIL_CLIENT_ID=…  FRANCE_TRAVAIL_CLIENT_SECRET=…"));
        self.ui.line(&(text("  Adzuna (optional, free tier): ") + &cyan("https://developer.adzuna.com/signup")));
        self.ui.line(&muted("    export ADZUNA_APP_ID=…  ADZUNA_APP_KEY=…"));
    }
}

fn skill_row(rank: usize, s: &Stat) -> String {
    let mut row = format!(
        "{}{}{} {}  ",
        neon(format!("{rank:02} ")),
        title(pad_right(s.name, 26)),
        bar(s.combined, 10),
        neon(percent(s.combined))
    );
    for share in &s.shares {
        row.push_str(&text(pad_left(&trimmed_percent(*share), 10)));
    }
    if let Some(d) = s.delta {
        row.push_str(&pad_left(&delta(d), 11));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{Harness, Setup, harness};
    use super::super::{Options, Outcome, Section};
    use crate::config::Config;
    use crate::httpx::testserver::{Request, Response, TestServer};

    const THREADS: &str = r#"{"hits":[
        {"objectID":"2","title":"Ask HN: Who is hiring? (October 2026)","created_at_i":1790000000},
        {"objectID":"1","title":"Ask HN: Who is hiring? (September 2026)","created_at_i":1787000000}]}"#;

    fn hn_item(posts: &[&str]) -> String {
        let children: Vec<String> =
            posts.iter().map(|p| format!(r#"{{"text":"{}","author":"a"}}"#, p.repeat(3))).collect();
        format!(r#"{{"children":[{}]}}"#, children.join(","))
    }

    fn ft_page() -> String {
        let offers: Vec<String> = (0..10)
            .map(|i| {
                format!(
                    r#"{{"id":"o{i}","intitule":"Développeur PHP React {i}","description":"Laravel, React, Docker, remote possible","dateCreation":"2026-10-04T08:00:00Z","lieuTravail":{{"libelle":"33 - LIBOURNE","latitude":44.9125,"longitude":-0.2328}},"entreprise":{{"nom":"Acme {i}"}},"typeContrat":"CDI","salaire":{{"libelle":"Annuel de 40000.00 Euros à 50000.00 Euros"}}}}"#
                )
            })
            .collect();
        format!(r#"{{"resultats":[{}]}}"#, offers.join(","))
    }

    fn market_server() -> TestServer {
        let october = hn_item(&[
            "Acme | Python and LLM agents engineer | Remote | ",
            "Beta | Product Engineer, TypeScript, React | Paris | ",
        ]);
        let september = hn_item(&["Gamma | Java and Spring backend developer | Onsite | "]);
        let page = ft_page();
        TestServer::start(move |req: &Request| {
            let path = req.path.split('?').next().unwrap_or_default();
            match path {
                "/threads" => Response::ok(THREADS),
                "/items/2" => Response::ok(&october),
                "/items/1" => Response::ok(&september),
                "/token" => Response::ok(r#"{"access_token":"tok","expires_in":1499}"#),
                "/search" => Response::ok(&page),
                p if p.starts_with("/table/v1/driving/") => {
                    let count = p.trim_start_matches("/table/v1/driving/").split(';').count();
                    let mut row = vec!["900".to_string(); count];
                    row[0] = "0".to_string();
                    Response::ok(&format!(r#"{{"code":"Ok","durations":[[{}]]}}"#, row.join(",")))
                }
                _ => Response::status(404, "missing"),
            }
        })
    }

    fn market_harness(srv: &TestServer, sections: &[Section], with_board: bool) -> Harness {
        let mut cfg = Config::default();
        cfg.jobs.routing_url = srv.url("");
        cfg.jobs.profile_skills = ["php", "laravel", "react", "docker"].map(String::from).to_vec();
        if with_board {
            cfg.france_travail.client_id = "id".into();
            cfg.france_travail.client_secret = "secret".into();
        }
        let mut h = harness(Setup { sections: sections.to_vec(), opts: Options::default(), cfg, ..Setup::default() });
        let e = &mut h.routine.endpoints;
        e.hn_threads = srv.url("/threads");
        e.hn_item = srv.url("/items/");
        e.ft_token = srv.url("/token");
        e.ft_search = srv.url("/search");
        h
    }

    #[test]
    fn jobs_without_credentials_show_setup_and_quick_links() {
        let srv = market_server();
        let mut h = market_harness(&srv, &[Section::Jobs], false);
        assert_eq!(h.routine.run(), Outcome::Completed, "missing credentials is a warning, not a failure");
        let plain = h.out.plain();
        for want in [
            "no job board configured",
            "francetravail.io",
            "FRANCE_TRAVAIL_CLIENT_ID",
            "France Travail https://candidat.francetravail.fr/offres/recherche?lieux=33063&motsCles=developpeur&offresPartenaires=true&rayon=50&tri=0",
            "LinkedIn https://www.linkedin.com/jobs/search/?distance=25&f_TPR=r604800&keywords=d%C3%A9veloppeur&location=Bordeaux%2C+Nouvelle-Aquitaine%2C+France",
            "jobs: no job board credentials",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
    }

    #[test]
    fn jobs_rank_routed_offers_from_the_board() {
        let srv = market_server();
        let mut h = market_harness(&srv, &[Section::Jobs], true);
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in [
            "10 offers scanned via France Travail · 10 within 45 min · OSRM road routing",
            "01 ▸ Développeur PHP React",
            " CDI  ⏱ 15 min",
            "Acme",
            "Acme 0 · 33 - LIBOURNE · 40k–50k €/an · 28h ago · France Travail",
            "match docker · laravel · php · react",
            "jobs: 5 targets locked (best: Développeur PHP React",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
        assert!(!plain.contains("Jobs by Adzuna"));
    }

    #[test]
    fn skills_combine_hn_national_and_local_corpora() {
        let srv = market_server();
        let mut h = market_harness(&srv, &[Section::Skills], true);
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        for want in [
            "corpus: HN hiring October 2026 (2 posts) · France national (10 posts) · ≤45min local (10 posts)",
            "trend Δ: HN hiring October 2026 vs HN hiring September 2026",
            "Δ m/m",
            "React",
            "⌁ market signals",
            "AI in the job description",
            "fastest riser / faller",
            "skills: top 5 → ",
        ] {
            assert!(plain.contains(want), "missing {want:?}\n{plain}");
        }
    }

    #[test]
    fn skills_without_any_source_warn() {
        let srv = TestServer::start(|_| Response::status(503, "down"));
        let mut h = market_harness(&srv, &[Section::Skills], false);
        assert_eq!(h.routine.run(), Outcome::Completed);
        let plain = h.out.plain();
        assert!(plain.contains("HN hiring threads offline: GET "), "{plain}");
        assert!(plain.contains("no corpus available, skills analysis skipped"), "{plain}");
        assert!(plain.contains("skills: no data source reachable"), "{plain}");
    }
}
