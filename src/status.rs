//! A domain's effective state: the inventory entry combined with what RDAP
//! and Cloudflare report, and the settings' price table.

use jiff::{SignedDuration, Timestamp};

use crate::cloudflare::CfDomain;
use crate::facts::{Check, FactStore};
use crate::inventory::{Declared, Price, Snapshot};
use crate::rdap::RdapFacts;
use crate::settings::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub warn_before: SignedDuration,
    pub critical_before: SignedDuration,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            warn_before: SignedDuration::from_hours(30 * 24),
            critical_before: SignedDuration::from_hours(7 * 24),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    Expired,
    Critical,
    Warning,
    Unknown,
    Ok,
}

impl State {
    pub const ALL: [State; 5] = [
        State::Expired,
        State::Critical,
        State::Warning,
        State::Unknown,
        State::Ok,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Expired => "expired",
            State::Critical => "critical",
            State::Warning => "warning",
            State::Unknown => "unknown",
            State::Ok => "ok",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Expired => "Expired",
            State::Critical => "Critical",
            State::Warning => "Due soon",
            State::Unknown => "Unknown",
            State::Ok => "OK",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.as_str() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Cloudflare,
    Rdap,
    Inventory,
    PriceTable,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Cloudflare => "Cloudflare",
            Source::Rdap => "RDAP",
            Source::Inventory => "inventory",
            Source::PriceTable => "price table",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DomainStatus {
    pub declared: Declared,
    pub registrar_name: String,
    pub registrar_url: Option<String>,
    pub expires_at: Option<(Timestamp, Source)>,
    pub auto_renew: Option<(bool, Source)>,
    pub price: Option<(Price, Source)>,
    pub thresholds: Thresholds,
    pub rdap: Option<Check<RdapFacts>>,
    /// The account's entry, when the domain is in it.
    pub cloudflare: Option<CfDomain>,
    /// RDAP names a registrar that doesn't match the declared one.
    pub registrar_mismatch: Option<String>,
    /// Declared as a Cloudflare domain but absent from a successful listing.
    pub missing_from_cloudflare: bool,
}

impl DomainStatus {
    pub fn compute(
        declared: &Declared,
        facts: &FactStore,
        settings: &Settings,
        defaults: Thresholds,
    ) -> DomainStatus {
        let rdap = facts.rdap(&declared.name);
        let rdap_facts = rdap.as_ref().and_then(|c| c.value());
        let cf_listing = facts.cloudflare();
        let cf_domains = cf_listing.as_ref().and_then(|c| c.value());
        let cloudflare = cf_domains.and_then(|d| d.get(&declared.name)).cloned();
        let is_cf = declared.registrar == "cloudflare";
        // Only trust the account's data for domains declared there.
        let cf_for_expiry = cloudflare.as_ref().filter(|_| is_cf);

        let expires_at = cf_for_expiry
            .and_then(|c| c.expires_at)
            .map(|t| (t, Source::Cloudflare))
            .or_else(|| {
                rdap_facts
                    .and_then(|f| f.expires_at)
                    .map(|t| (t, Source::Rdap))
            })
            .or_else(|| declared.expires_at.map(|t| (t, Source::Inventory)));
        let auto_renew = cf_for_expiry
            .and_then(|c| c.auto_renew)
            .map(|v| (v, Source::Cloudflare))
            .or_else(|| declared.auto_renew.map(|v| (v, Source::Inventory)));
        let price = declared
            .renewal
            .clone()
            .map(|p| (p, Source::Inventory))
            .or_else(|| {
                settings
                    .table_price(declared)
                    .map(|p| (p, Source::PriceTable))
            });
        let registrar_mismatch = rdap_facts
            .and_then(|f| f.registrar.as_ref())
            .filter(|r| !settings.rdap_matches(&declared.registrar, r))
            .cloned();
        let missing_from_cloudflare = is_cf && cf_domains.is_some() && cloudflare.is_none();

        DomainStatus {
            registrar_name: settings.registrar_name(&declared.registrar),
            registrar_url: settings.registrar_url(&declared.registrar),
            expires_at,
            auto_renew,
            price,
            thresholds: Thresholds {
                warn_before: declared.warn_before.unwrap_or(defaults.warn_before),
                critical_before: declared.critical_before.unwrap_or(defaults.critical_before),
            },
            rdap,
            cloudflare,
            registrar_mismatch,
            missing_from_cloudflare,
            declared: declared.clone(),
        }
    }

    pub fn name(&self) -> &str {
        &self.declared.name
    }

    pub fn remaining(&self, now: Timestamp) -> Option<SignedDuration> {
        self.expires_at.map(|(t, _)| t.duration_since(now))
    }

    pub fn auto_renews(&self) -> Option<bool> {
        self.auto_renew.map(|(v, _)| v)
    }

    /// Warning is skipped when auto-renew is known to be on; critical is not:
    /// that close to expiry, an auto-renewal has failed.
    pub fn state(&self, now: Timestamp) -> State {
        match self.remaining(now) {
            None => State::Unknown,
            Some(r) if r <= SignedDuration::ZERO => State::Expired,
            Some(r) if r < self.thresholds.critical_before => State::Critical,
            Some(r) if r < self.thresholds.warn_before && self.auto_renews() != Some(true) => {
                State::Warning
            }
            Some(_) => State::Ok,
        }
    }

    pub fn has_drift(&self) -> bool {
        self.registrar_mismatch.is_some() || self.missing_from_cloudflare
    }
}

/// Every declared domain's status, plus the Cloudflare domains nobody declared.
pub struct Overview {
    pub domains: Vec<DomainStatus>,
    pub undeclared: Vec<String>,
    pub snapshot: Snapshot,
}

impl Overview {
    pub fn compute(
        snapshot: Snapshot,
        facts: &FactStore,
        settings: &Settings,
        defaults: Thresholds,
    ) -> Overview {
        let domains = snapshot
            .domains
            .iter()
            .map(|d| DomainStatus::compute(d, facts, settings, defaults))
            .collect();
        let undeclared = facts
            .cloudflare()
            .and_then(|c| c.value().cloned())
            .map(|cf| {
                cf.into_keys()
                    .filter(|name| snapshot.get(name).is_none())
                    .collect()
            })
            .unwrap_or_default();
        Overview {
            domains,
            undeclared,
            snapshot,
        }
    }
}

/// Everything needed to compute an [`Overview`], shared by the UI and metrics.
pub struct Domains {
    pub inventory: std::sync::Arc<dyn crate::inventory::Inventory>,
    pub facts: std::sync::Arc<FactStore>,
    pub settings: Settings,
    pub thresholds: Thresholds,
}

impl Domains {
    pub fn overview(&self) -> Overview {
        Overview::compute(
            self.inventory.snapshot(),
            &self.facts,
            &self.settings,
            self.thresholds,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::CfDomains;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn declared(name: &str, registrar: &str) -> Declared {
        Declared {
            name: name.into(),
            registrar: registrar.into(),
            purpose: None,
            tags: vec![],
            notes: None,
            auto_renew: None,
            renewal: None,
            expires_at: None,
            warn_before: None,
            critical_before: None,
            source: "inv/d.json".into(),
        }
    }

    fn rdap(expires: &str, registrar: &str) -> RdapFacts {
        RdapFacts {
            expires_at: Some(ts(expires)),
            registrar: Some(registrar.into()),
            ..Default::default()
        }
    }

    fn cf(name: &str, expires: &str, auto_renew: bool) -> CfDomains {
        CfDomains::from([(
            name.to_string(),
            CfDomain {
                expires_at: Some(ts(expires)),
                auto_renew: Some(auto_renew),
                ..Default::default()
            },
        )])
    }

    #[test]
    fn precedence_cloudflare_then_rdap_then_inventory() {
        let facts = FactStore::new(true);
        let settings = Settings::default();
        let now = ts("2026-09-26T00:00:00Z");
        let mut d = declared("a.com", "cloudflare");
        d.expires_at = Some(ts("2026-01-01T00:00:00Z"));
        d.auto_renew = Some(false);

        let s = DomainStatus::compute(&d, &facts, &settings, Thresholds::default());
        assert_eq!(
            s.expires_at,
            Some((ts("2026-01-01T00:00:00Z"), Source::Inventory))
        );
        assert_eq!(s.auto_renew, Some((false, Source::Inventory)));

        facts.record_rdap(
            "a.com",
            now,
            Ok(rdap("2027-02-01T00:00:00Z", "Cloudflare, Inc.")),
        );
        let s = DomainStatus::compute(&d, &facts, &settings, Thresholds::default());
        assert_eq!(s.expires_at.unwrap().1, Source::Rdap);

        facts.record_cloudflare(now, Ok(cf("a.com", "2027-03-01T00:00:00Z", true)));
        let s = DomainStatus::compute(&d, &facts, &settings, Thresholds::default());
        assert_eq!(
            s.expires_at,
            Some((ts("2027-03-01T00:00:00Z"), Source::Cloudflare))
        );
        assert_eq!(s.auto_renew, Some((true, Source::Cloudflare)));
        assert!(!s.has_drift());
    }

    #[test]
    fn states_respect_auto_renew() {
        let facts = FactStore::new(false);
        let settings = Settings::default();
        let mut d = declared("a.com", "porkbun");
        d.expires_at = Some(ts("2026-10-15T00:00:00Z"));
        let at = |now: &str, d: &Declared| {
            DomainStatus::compute(d, &facts, &settings, Thresholds::default()).state(ts(now))
        };
        assert_eq!(at("2026-08-01T00:00:00Z", &d), State::Ok);
        assert_eq!(at("2026-09-26T00:00:00Z", &d), State::Warning);
        assert_eq!(at("2026-10-10T00:00:00Z", &d), State::Critical);
        assert_eq!(at("2026-10-15T00:00:00Z", &d), State::Expired);
        d.auto_renew = Some(true);
        assert_eq!(at("2026-09-26T00:00:00Z", &d), State::Ok);
        assert_eq!(at("2026-10-10T00:00:00Z", &d), State::Critical);
        d.expires_at = None;
        assert_eq!(at("2026-09-26T00:00:00Z", &d), State::Unknown);
    }

    #[test]
    fn drift_and_undeclared() {
        let facts = FactStore::new(true);
        let settings = Settings::default();
        let now = ts("2026-09-26T00:00:00Z");
        facts.record_cloudflare(now, Ok(cf("stray.dev", "2027-01-01T00:00:00Z", true)));
        facts.record_rdap(
            "moved.com",
            now,
            Ok(rdap("2027-01-01T00:00:00Z", "Cloudflare, Inc.")),
        );
        let snapshot = Snapshot {
            domains: vec![
                declared("moved.com", "namecheap"),
                declared("gone.com", "cloudflare"),
            ],
            problems: vec![],
        };
        let o = Overview::compute(snapshot, &facts, &settings, Thresholds::default());
        assert_eq!(o.undeclared, vec!["stray.dev"]);
        let moved = &o.domains[0];
        assert_eq!(
            moved.registrar_mismatch.as_deref(),
            Some("Cloudflare, Inc.")
        );
        assert!(!moved.missing_from_cloudflare);
        // Cloudflare's data is not used for a domain declared elsewhere.
        assert_eq!(moved.expires_at.unwrap().1, Source::Rdap);
        assert!(o.domains[1].missing_from_cloudflare);
    }

    #[test]
    fn prices_and_thresholds() {
        let facts = FactStore::new(false);
        let mut settings = Settings::default();
        let cf = settings.registrars.get_mut("cloudflare").unwrap();
        cf.prices.insert("com".into(), 10.44);
        let mut d = declared("a.com", "cloudflare");
        d.warn_before = Some(SignedDuration::from_hours(24));
        let s = DomainStatus::compute(&d, &facts, &settings, Thresholds::default());
        assert_eq!(s.price.as_ref().unwrap().1, Source::PriceTable);
        assert_eq!(s.price.unwrap().0.currency, "USD");
        assert_eq!(s.thresholds.warn_before, SignedDuration::from_hours(24));
        d.renewal = Some(Price {
            amount: 9.0,
            currency: "EUR".into(),
        });
        let s = DomainStatus::compute(&d, &facts, &settings, Thresholds::default());
        assert_eq!(s.price.unwrap().1, Source::Inventory);
        assert_eq!(State::parse("warning"), Some(State::Warning));
    }
}
