use serde::Deserialize;

use super::{Offer, Point};
use crate::httpx::{Client, query_string};

const EARTH_RADIUS_KM: f64 = 6371.0;
const ROAD_DETOUR_FACTOR: f64 = 1.35;
const AVERAGE_SPEED_KMH: f64 = 65.0;
const OSRM_MAX_COORDINATES: usize = 100;

pub fn haversine_km(a: Point, b: Point) -> f64 {
    let (d_lat, d_lon) = ((b.lat - a.lat).to_radians(), (b.lon - a.lon).to_radians());
    let h =
        (d_lat / 2.0).sin().powi(2) + a.lat.to_radians().cos() * b.lat.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().asin()
}

pub fn estimate_drive_minutes(a: Point, b: Point) -> f64 {
    haversine_km(a, b) * ROAD_DETOUR_FACTOR / AVERAGE_SPEED_KMH * 60.0
}

pub struct Osrm {
    pub base_url: String,
    pub client: Client,
}

#[derive(Deserialize)]
struct Table {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    durations: Vec<Vec<Option<f64>>>,
}

impl Osrm {
    /// Origin→destination durations from the OSRM table service, chunked under the coordinate limit; `None` is unroutable.
    pub fn drive_minutes(&self, origin: Point, dests: &[Point]) -> Result<Vec<Option<f64>>, String> {
        let mut out = Vec::with_capacity(dests.len());
        for chunk in dests.chunks(OSRM_MAX_COORDINATES - 1) {
            out.extend(self.table(origin, chunk)?);
        }
        Ok(out)
    }

    fn table(&self, origin: Point, dests: &[Point]) -> Result<Vec<Option<f64>>, String> {
        let coords: Vec<String> =
            std::iter::once(&origin).chain(dests).map(|p| format!("{:.5},{:.5}", p.lon, p.lat)).collect();
        let endpoint = format!(
            "{}/table/v1/driving/{}?{}",
            self.base_url.trim_end_matches('/'),
            coords.join(";"),
            query_string(&[("sources", "0"), ("annotations", "duration")])
        );
        let table: Table = self.client.get_json(&endpoint, &[]).map_err(|e| e.to_string())?;
        if table.code != "Ok" {
            return Err(format!("osrm: {} {}", table.code, table.message));
        }
        match table.durations.as_slice() {
            [row] if row.len() == dests.len() + 1 => Ok(row[1..].iter().map(|s| s.map(|secs| secs / 60.0)).collect()),
            _ => Err("osrm: unexpected matrix shape".to_string()),
        }
    }
}

/// Uses OSRM when reachable and falls back to a straight-line estimate, flagged as such. Returns whether OSRM answered.
pub fn attach_drive_times(router: Option<&Osrm>, origin: Point, offers: &mut [Offer]) -> bool {
    let located: Vec<(usize, Point)> =
        offers.iter().enumerate().filter_map(|(i, o)| o.coords.map(|p| (i, p))).collect();
    if located.is_empty() {
        return false;
    }
    let dests: Vec<Point> = located.iter().map(|(_, p)| *p).collect();
    let routed = router.and_then(|r| r.drive_minutes(origin, &dests).ok());
    for (k, (i, dest)) in located.into_iter().enumerate() {
        match routed.as_ref().and_then(|minutes| minutes[k]) {
            Some(minutes) => offers[i].drive_minutes = Some(minutes),
            None => {
                offers[i].drive_minutes = Some(estimate_drive_minutes(origin, dest));
                offers[i].drive_estimated = true;
            }
        }
    }
    routed.is_some()
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::httpx::testserver::{Response, TestServer};

    pub const BORDEAUX: Point = Point { lat: 44.8624, lon: -0.5848 };
    pub const LIBOURNE: Point = Point { lat: 44.9125, lon: -0.2328 };
    pub const BERGERAC: Point = Point { lat: 44.8519, lon: 0.4883 };

    fn router(srv: &TestServer) -> Osrm {
        Osrm { base_url: srv.url(""), client: Client::new("test", None) }
    }

    #[test]
    fn haversine_and_estimate() {
        let km = haversine_km(BORDEAUX, LIBOURNE);
        assert!((27.0..30.0).contains(&km), "{km}");
        assert_eq!(haversine_km(BORDEAUX, BORDEAUX), 0.0);
        let minutes = estimate_drive_minutes(BORDEAUX, BERGERAC);
        assert!((60.0..120.0).contains(&minutes), "{minutes}");
    }

    #[test]
    fn table_request_and_unroutable_entries() {
        let srv = TestServer::start(|req| {
            assert!(req.path.starts_with("/table/v1/driving/-0.58480,44.86240;-0.23280,44.91250;"), "{}", req.path);
            assert!(req.path.ends_with("?annotations=duration&sources=0"), "{}", req.path);
            Response::ok(r#"{"code":"Ok","durations":[[0,900,null]]}"#)
        });
        assert_eq!(router(&srv).drive_minutes(BORDEAUX, &[LIBOURNE, BERGERAC]).unwrap(), [Some(15.0), None]);
    }

    #[test]
    fn table_errors() {
        let bad = TestServer::start(|_| Response::ok(r#"{"code":"InvalidQuery","message":"bad coords"}"#));
        assert!(router(&bad).drive_minutes(BORDEAUX, &[LIBOURNE]).unwrap_err().contains("InvalidQuery"));
        let shape = TestServer::start(|_| Response::ok(r#"{"code":"Ok","durations":[[0]]}"#));
        assert!(router(&shape).drive_minutes(BORDEAUX, &[LIBOURNE]).is_err());
    }

    #[test]
    fn large_requests_are_chunked() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let srv = TestServer::start(move |req| {
            seen.fetch_add(1, Ordering::SeqCst);
            let path = req.path.split('?').next().unwrap_or_default();
            let coords = path.trim_start_matches("/table/v1/driving/").split(';').count();
            assert!(coords <= OSRM_MAX_COORDINATES, "{coords} coordinates");
            Response::ok(&format!(r#"{{"code":"Ok","durations":[[{}]]}}"#, vec!["60"; coords].join(",")))
        });
        let minutes = router(&srv).drive_minutes(BORDEAUX, &[LIBOURNE; 150]).unwrap();
        assert_eq!((minutes.len(), calls.load(Ordering::SeqCst)), (150, 2));
    }

    #[test]
    fn falls_back_to_estimates() {
        let down = TestServer::start(|_| Response::status(502, "down"));
        let mut offers = vec![Offer { coords: Some(LIBOURNE), ..Offer::default() }, Offer::default()];
        assert!(!attach_drive_times(Some(&router(&down)), BORDEAUX, &mut offers));
        assert_eq!(offers[0].drive_minutes, Some(estimate_drive_minutes(BORDEAUX, LIBOURNE)));
        assert!(offers[0].drive_estimated);
        assert_eq!(offers[1].drive_minutes, None, "no coordinates, no drive time");
    }

    #[test]
    fn unroutable_destinations_are_estimated() {
        let srv = TestServer::start(|_| Response::ok(r#"{"code":"Ok","durations":[[0,1200,null]]}"#));
        let mut offers = vec![
            Offer { coords: Some(LIBOURNE), ..Offer::default() },
            Offer { coords: Some(BERGERAC), ..Offer::default() },
        ];
        assert!(attach_drive_times(Some(&router(&srv)), BORDEAUX, &mut offers));
        assert_eq!((offers[0].drive_minutes, offers[0].drive_estimated), (Some(20.0), false));
        assert!(offers[1].drive_estimated);
    }
}
