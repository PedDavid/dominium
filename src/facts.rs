//! In-memory cache of what RDAP and Cloudflare report, and the background
//! task that keeps it fresh. Nothing is persisted: after a restart every
//! domain is looked up again.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tracing::{debug, info, warn};

use crate::cloudflare::{CfDomains, Registrar};
use crate::inventory::Inventory;
use crate::metrics::Counters;
use crate::rdap::{RdapError, RdapFacts, RdapLookup};

/// The latest answer from a source. `last_ok` survives later failures, so a
/// flaky RDAP server doesn't make a known expiry disappear.
#[derive(Clone, Debug)]
pub struct Check<T> {
    pub at: Timestamp,
    pub last_ok: Option<(Timestamp, T)>,
    pub error: Option<String>,
}

impl<T> Check<T> {
    pub fn value(&self) -> Option<&T> {
        self.last_ok.as_ref().map(|(_, v)| v)
    }
}

fn record<T>(slot: &mut Option<Check<T>>, at: Timestamp, result: Result<T, String>) {
    let last_ok = slot.take().and_then(|c| c.last_ok);
    *slot = Some(match result {
        Ok(v) => Check {
            at,
            last_ok: Some((at, v)),
            error: None,
        },
        Err(e) => Check {
            at,
            last_ok,
            error: Some(e),
        },
    });
}

#[derive(Default)]
struct Inner {
    rdap: HashMap<String, Check<RdapFacts>>,
    cloudflare: Option<Check<CfDomains>>,
}

#[derive(Default)]
pub struct FactStore {
    inner: RwLock<Inner>,
    cloudflare_enabled: bool,
}

impl FactStore {
    pub fn new(cloudflare_enabled: bool) -> Arc<Self> {
        Arc::new(FactStore {
            inner: RwLock::default(),
            cloudflare_enabled,
        })
    }

    pub fn cloudflare_enabled(&self) -> bool {
        self.cloudflare_enabled
    }

    pub fn rdap(&self, domain: &str) -> Option<Check<RdapFacts>> {
        self.inner.read().unwrap().rdap.get(domain).cloned()
    }

    pub fn cloudflare(&self) -> Option<Check<CfDomains>> {
        self.inner.read().unwrap().cloudflare.clone()
    }

    pub fn record_rdap(&self, domain: &str, at: Timestamp, result: Result<RdapFacts, String>) {
        let mut inner = self.inner.write().unwrap();
        let mut slot = inner.rdap.remove(domain);
        record(&mut slot, at, result);
        if let Some(c) = slot {
            inner.rdap.insert(domain.to_string(), c);
        }
    }

    pub fn record_cloudflare(&self, at: Timestamp, result: Result<CfDomains, String>) {
        record(&mut self.inner.write().unwrap().cloudflare, at, result);
    }

    /// Drops cached answers for domains no longer in the inventory.
    pub fn retain_rdap(&self, keep: impl Fn(&str) -> bool) {
        self.inner.write().unwrap().rdap.retain(|k, _| keep(k));
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Intervals {
    pub rdap: SignedDuration,
    pub rdap_retry: SignedDuration,
    pub cloudflare: SignedDuration,
    /// Pause between two RDAP requests, to stay polite with registries.
    pub rdap_spacing: Duration,
    pub tick: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Intervals {
            rdap: SignedDuration::from_hours(12),
            rdap_retry: SignedDuration::from_hours(1),
            cloudflare: SignedDuration::from_hours(1),
            rdap_spacing: Duration::from_secs(2),
            tick: Duration::from_secs(60),
        }
    }
}

/// A stable per-domain offset of up to 10% of the interval, so domains added
/// together don't stay in lockstep.
fn spread(domain: &str, interval: SignedDuration) -> SignedDuration {
    let mut h = DefaultHasher::new();
    domain.hash(&mut h);
    let tenth = (interval.as_secs() / 10).max(1) as u64;
    SignedDuration::from_secs((h.finish() % tenth) as i64)
}

/// Whether a domain's RDAP data should be fetched again.
pub fn rdap_due(
    check: Option<&Check<RdapFacts>>,
    domain: &str,
    i: &Intervals,
    now: Timestamp,
) -> bool {
    match check {
        None => true,
        Some(c) => {
            let wait = if c.error.is_some() {
                i.rdap_retry
            } else {
                i.rdap - spread(domain, i.rdap)
            };
            now.duration_since(c.at) >= wait
        }
    }
}

pub struct Refresher {
    pub inventory: Arc<dyn Inventory>,
    pub facts: Arc<FactStore>,
    pub rdap: Arc<dyn RdapLookup>,
    pub cloudflare: Option<Arc<dyn Registrar>>,
    pub counters: Arc<Counters>,
    pub intervals: Intervals,
}

impl Refresher {
    pub fn spawn(self) {
        tokio::spawn(async move {
            loop {
                self.run_once().await;
                tokio::time::sleep(self.intervals.tick).await;
            }
        });
    }

    /// One pass: Cloudflare if due, then every domain whose RDAP data is due.
    pub async fn run_once(&self) {
        if !self.inventory.ready() {
            debug!("inventory not synced yet");
            return;
        }
        if let Some(cf) = &self.cloudflare {
            let due = self
                .facts
                .cloudflare()
                .is_none_or(|c| Timestamp::now().duration_since(c.at) >= self.intervals.cloudflare);
            if due {
                let result = cf.list().await;
                match &result {
                    Ok(d) => info!(domains = d.len(), "Cloudflare registrar listed"),
                    Err(e) => warn!(error = %e, "Cloudflare registrar listing failed"),
                }
                self.counters
                    .cloudflare(if result.is_ok() { "ok" } else { "error" });
                self.facts.record_cloudflare(Timestamp::now(), result);
            }
        }

        let snapshot = self.inventory.snapshot();
        self.facts.retain_rdap(|name| snapshot.get(name).is_some());
        let mut first = true;
        for domain in &snapshot.domains {
            let now = Timestamp::now();
            if !rdap_due(
                self.facts.rdap(&domain.name).as_ref(),
                &domain.name,
                &self.intervals,
                now,
            ) {
                continue;
            }
            if !first {
                tokio::time::sleep(self.intervals.rdap_spacing).await;
            }
            first = false;
            let result = self.rdap.lookup(&domain.name).await;
            let label = match &result {
                Ok(_) => "ok",
                Err(e) => e.label(),
            };
            self.counters.rdap(label);
            match &result {
                Ok(f) => debug!(domain = %domain.name, expires = ?f.expires_at, "RDAP lookup"),
                Err(RdapError::NoServer(_)) => debug!(domain = %domain.name, "no RDAP server"),
                Err(e) => warn!(domain = %domain.name, error = %e, "RDAP lookup failed"),
            }
            self.facts.record_rdap(
                &domain.name,
                Timestamp::now(),
                result.map_err(|e| e.to_string()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{Snapshot, StaticInventory, from_configmaps};
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct FakeRdap(Mutex<Vec<String>>);

    #[async_trait]
    impl RdapLookup for FakeRdap {
        async fn lookup(&self, domain: &str) -> Result<RdapFacts, RdapError> {
            self.0.lock().unwrap().push(domain.to_string());
            if domain.ends_with(".pt") {
                return Err(RdapError::NoServer("pt".into()));
            }
            Ok(RdapFacts {
                registrar: Some("Cloudflare, Inc.".into()),
                ..Default::default()
            })
        }
    }

    fn snapshot() -> Snapshot {
        let data = BTreeMap::from([(
            "d.json".to_string(),
            r#"{"version":1,"domains":[{"name":"a.com","registrar":"cloudflare"},{"name":"b.pt","registrar":"ptisp"}]}"#.to_string(),
        )]);
        from_configmaps([("inv", &data)])
    }

    #[tokio::test]
    async fn refreshes_due_domains_only() {
        let rdap = Arc::new(FakeRdap(Mutex::default()));
        let facts = FactStore::new(false);
        let refresher = Refresher {
            inventory: Arc::new(StaticInventory(snapshot())),
            facts: facts.clone(),
            rdap: rdap.clone(),
            cloudflare: None,
            counters: Arc::new(Counters::default()),
            intervals: Intervals {
                rdap_spacing: Duration::ZERO,
                ..Default::default()
            },
        };
        refresher.run_once().await;
        assert_eq!(*rdap.0.lock().unwrap(), vec!["a.com", "b.pt"]);
        let a = facts.rdap("a.com").unwrap();
        assert!(a.value().is_some() && a.error.is_none());
        let b = facts.rdap("b.pt").unwrap();
        assert!(b.value().is_none() && b.error.unwrap().contains(".pt"));

        // Nothing is due right after a pass.
        refresher.run_once().await;
        assert_eq!(rdap.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn failures_keep_the_last_good_answer() {
        let facts = FactStore::new(false);
        let t0: Timestamp = "2026-09-01T00:00:00Z".parse().unwrap();
        let t1: Timestamp = "2026-09-02T00:00:00Z".parse().unwrap();
        facts.record_rdap("a.com", t0, Ok(RdapFacts::default()));
        facts.record_rdap("a.com", t1, Err("boom".into()));
        let c = facts.rdap("a.com").unwrap();
        assert_eq!(c.at, t1);
        assert_eq!(c.last_ok.as_ref().unwrap().0, t0);
        assert_eq!(c.error.as_deref(), Some("boom"));
    }

    #[test]
    fn due_times() {
        let i = Intervals::default();
        let now: Timestamp = "2026-09-26T12:00:00Z".parse().unwrap();
        let check = |at: Timestamp, error: bool| Check {
            at,
            last_ok: None,
            error: error.then(|| "x".to_string()),
        };
        assert!(rdap_due(None, "a.com", &i, now));
        let recent = now - SignedDuration::from_mins(30);
        assert!(!rdap_due(Some(&check(recent, false)), "a.com", &i, now));
        assert!(!rdap_due(Some(&check(recent, true)), "a.com", &i, now));
        let hours_ago = now - SignedDuration::from_hours(2);
        assert!(rdap_due(Some(&check(hours_ago, true)), "a.com", &i, now));
        assert!(!rdap_due(Some(&check(hours_ago, false)), "a.com", &i, now));
        let day_ago = now - SignedDuration::from_hours(12);
        assert!(rdap_due(Some(&check(day_ago, false)), "a.com", &i, now));
        assert!(spread("a.com", i.rdap) < SignedDuration::from_mins(73));
    }
}
