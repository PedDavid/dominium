//! The demo app at a fixed moment, shared by the snapshot and visual tests.

use std::sync::Arc;

use jiff::Timestamp;

use dominium::demo;
use dominium::inventory::StaticInventory;
use dominium::status::{Domains, Thresholds};
use dominium::web::{AppState, router};

/// Every relative date ("in 19d", "42m ago") is computed against this.
pub const NOW: &str = "2026-09-26T12:00:00Z";

pub fn app() -> axum::Router {
    let now: Timestamp = NOW.parse().unwrap();
    let domains = Arc::new(Domains {
        inventory: Arc::new(StaticInventory(demo::inventory(now))),
        facts: demo::facts(now),
        settings: demo::settings(),
        thresholds: Thresholds::default(),
    });
    router(AppState::with_fixed_now(domains, true, now))
}
