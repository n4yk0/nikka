use std::sync::Arc;
use std::thread;

use super::{Board, Offer, Osrm, Query, RankOptions, attach_drive_times, rank};

#[derive(Clone, Debug, Default)]
pub struct Market {
    pub ranked: Vec<Offer>,
    /// Every ranked offer inside the drive budget, before the display limit.
    pub local: Vec<Offer>,
    pub considered: usize,
    pub providers: Vec<String>,
    pub failures: Vec<String>,
    pub routed: bool,
}

/// Queries every board in parallel, attaches drive times and ranks the offers inside the budget.
pub fn scan(boards: &[Arc<dyn Board>], q: &Query, router: Option<&Osrm>, opts: &RankOptions) -> Market {
    let results: Vec<Result<Vec<Offer>, String>> = thread::scope(|s| {
        let handles: Vec<_> = boards.iter().map(|b| s.spawn(move || b.search(q))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("search panicked".to_string()))).collect()
    });
    let mut market = Market { providers: boards.iter().map(|b| b.name().to_string()).collect(), ..Market::default() };
    let mut all = Vec::new();
    for (board, result) in boards.iter().zip(results) {
        match result {
            Ok(offers) => all.extend(offers),
            Err(e) => market.failures.push(format!("{}: {e}", board.name())),
        }
    }
    market.considered = all.len();
    market.routed = attach_drive_times(router, q.origin, &mut all);
    market.local = rank(&all, &RankOptions { limit: 0, ..opts.clone() });
    market.ranked = market.local.iter().take(if opts.limit > 0 { opts.limit } else { usize::MAX }).cloned().collect();
    market
}

#[cfg(test)]
mod tests {
    use super::super::route::tests::{BERGERAC, BORDEAUX, LIBOURNE};
    use super::super::tests::now;
    use super::super::{CONTRACT_CDI, Point};
    use super::*;

    struct Stub {
        name: &'static str,
        offers: Result<Vec<Offer>, String>,
    }

    impl Board for Stub {
        fn name(&self) -> &'static str {
            self.name
        }
        fn search(&self, _: &Query) -> Result<Vec<Offer>, String> {
            self.offers.clone()
        }
        fn sample(&self, _: &Query, _: usize) -> Result<Vec<Offer>, String> {
            Ok(Vec::new())
        }
    }

    fn located(title: &str, company: &str, at: Point) -> Offer {
        Offer {
            title: title.into(),
            company: company.into(),
            contract: CONTRACT_CDI.into(),
            coords: Some(at),
            posted: Some(now()),
            ..Offer::default()
        }
    }

    #[test]
    fn merges_boards_and_reports_failures() {
        let boards: Vec<Arc<dyn Board>> = vec![
            Arc::new(Stub {
                name: "one",
                offers: Ok(vec![located("Développeur PHP", "a", LIBOURNE), located("Développeur Go", "b", BERGERAC)]),
            }),
            Arc::new(Stub { name: "two", offers: Err("deadline exceeded".into()) }),
        ];
        let q = Query { origin: BORDEAUX, ..Query::default() };
        let opts = RankOptions { now: now(), max_drive_minutes: 45.0, profile_skills: Vec::new(), limit: 5 };
        let market = scan(&boards, &q, None, &opts);
        assert_eq!(market.considered, 2);
        assert_eq!(market.local.iter().map(|o| o.title.as_str()).collect::<Vec<_>>(), ["Développeur PHP"]);
        assert_eq!(market.failures, ["two: deadline exceeded"]);
        assert_eq!(market.providers, ["one", "two"]);
        assert!(!market.routed);
    }
}
