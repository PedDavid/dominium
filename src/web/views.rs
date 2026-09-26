//! View models: domain statuses turned into pre-formatted rows for the templates.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};

use crate::duration::humanize;
use crate::inventory::Price;
use crate::status::{DomainStatus, Source, State};

pub fn fmt_date(t: Timestamp) -> String {
    t.strftime("%Y-%m-%d").to_string()
}

pub fn fmt_datetime(t: Timestamp) -> String {
    t.strftime("%Y-%m-%d %H:%M UTC").to_string()
}

/// "in 12d", "3d ago", "now".
pub fn relative(t: Timestamp, now: Timestamp) -> String {
    let d = t.duration_since(now);
    if d.abs() < SignedDuration::from_secs(60) {
        "now".into()
    } else if d.is_positive() {
        format!("in {}", humanize(d))
    } else {
        format!("{} ago", humanize(d))
    }
}

/// Only http(s) URLs are rendered as links (never `javascript:` and co).
pub fn safe_link(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

pub fn fmt_money(amount: f64, currency: &str) -> String {
    match currency {
        "USD" => format!("${amount:.2}"),
        "EUR" => format!("€{amount:.2}"),
        "GBP" => format!("£{amount:.2}"),
        _ => format!("{amount:.2} {currency}"),
    }
}

pub fn fmt_price(p: &Price) -> String {
    fmt_money(p.amount, &p.currency)
}

#[derive(Clone, Debug)]
pub struct DomainRow {
    pub name: String,
    pub purpose: String,
    pub tags: Vec<String>,
    pub registrar: String,
    pub registrar_name: String,
    pub manage_url: Option<String>,
    pub state: State,
    pub expires_at: Option<Timestamp>,
    pub expires_date: String,
    pub expires_rel: String,
    pub expires_source: &'static str,
    /// "on", "off" or "unknown".
    pub auto_renew: &'static str,
    pub price: Option<Price>,
    pub price_label: String,
    pub drift: bool,
}

impl DomainRow {
    pub fn new(d: &DomainStatus, now: Timestamp) -> DomainRow {
        let expires = d.expires_at.map(|(t, _)| t);
        DomainRow {
            name: d.declared.name.clone(),
            purpose: d.declared.purpose.clone().unwrap_or_default(),
            tags: d.declared.tags.clone(),
            registrar: d.declared.registrar.clone(),
            registrar_name: d.registrar_name.clone(),
            manage_url: d.registrar_url.as_deref().and_then(safe_link),
            state: d.state(now),
            expires_at: expires,
            expires_date: expires.map(fmt_date).unwrap_or_default(),
            expires_rel: expires
                .map(|t| relative(t, now))
                .unwrap_or_else(|| "—".into()),
            expires_source: d.expires_at.map(|(_, s)| s.label()).unwrap_or_default(),
            auto_renew: match d.auto_renews() {
                Some(true) => "on",
                Some(false) => "off",
                None => "unknown",
            },
            price: d.price.as_ref().map(|(p, _)| p.clone()),
            price_label: d
                .price
                .as_ref()
                .map(|(p, _)| fmt_price(p))
                .unwrap_or_default(),
            drift: d.has_drift(),
        }
    }

    pub fn state_str(&self) -> &'static str {
        self.state.as_str()
    }

    pub fn state_label(&self) -> &'static str {
        self.state.label()
    }

    pub fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        [&self.name, &self.purpose, &self.registrar_name]
            .into_iter()
            .chain(self.tags.iter())
            .any(|f| f.to_lowercase().contains(&q))
    }
}

/// Yearly cost per currency, largest first, e.g. `["$84.12", "€16.90"]`.
pub fn yearly_totals(rows: &[DomainRow]) -> Vec<String> {
    let mut totals: BTreeMap<&str, f64> = BTreeMap::new();
    for p in rows.iter().filter_map(|r| r.price.as_ref()) {
        *totals.entry(p.currency.as_str()).or_default() += p.amount;
    }
    let mut totals: Vec<(&str, f64)> = totals.into_iter().collect();
    totals.sort_by(|a, b| b.1.total_cmp(&a.1));
    totals.into_iter().map(|(c, a)| fmt_money(a, c)).collect()
}

pub struct DomainDetail {
    pub row: DomainRow,
    pub notes: String,
    pub source: String,
    pub expires_full: String,
    pub auto_renew_source: &'static str,
    pub price_source: &'static str,
    pub warn_before: String,
    pub critical_before: String,
    pub auto_renew_skips_warning: bool,
    pub inventory_expires: String,
    pub rdap_registrar: String,
    pub rdap_registered: String,
    pub rdap_updated: String,
    pub rdap_expires: String,
    pub rdap_statuses: Vec<String>,
    pub nameservers: Vec<String>,
    pub rdap_url: Option<String>,
    pub rdap_checked: String,
    pub rdap_last_ok: String,
    pub rdap_error: String,
    pub registrar_mismatch: String,
    pub missing_from_cloudflare: bool,
    pub cloudflare: Option<CloudflareRow>,
}

pub struct CloudflareRow {
    pub expires: String,
    pub auto_renew: String,
    pub locked: String,
    pub privacy: String,
}

fn yes_no(v: Option<bool>) -> String {
    match v {
        Some(true) => "yes".into(),
        Some(false) => "no".into(),
        None => "—".into(),
    }
}

impl DomainDetail {
    pub fn new(d: &DomainStatus, now: Timestamp) -> DomainDetail {
        let row = DomainRow::new(d, now);
        let rdap = d.rdap.as_ref();
        let facts = rdap.and_then(|c| c.value());
        let date = |t: Option<Timestamp>| t.map(fmt_date).unwrap_or_default();
        DomainDetail {
            notes: d.declared.notes.clone().unwrap_or_default(),
            source: d.declared.source.clone(),
            expires_full: d
                .expires_at
                .map(|(t, _)| fmt_datetime(t))
                .unwrap_or_default(),
            auto_renew_source: d.auto_renew.map(|(_, s)| s.label()).unwrap_or_default(),
            price_source: d.price.as_ref().map(|(_, s)| s.label()).unwrap_or_default(),
            warn_before: humanize(d.thresholds.warn_before),
            critical_before: humanize(d.thresholds.critical_before),
            auto_renew_skips_warning: d.auto_renews() == Some(true),
            inventory_expires: date(d.declared.expires_at),
            rdap_registrar: facts.and_then(|f| f.registrar.clone()).unwrap_or_default(),
            rdap_registered: date(facts.and_then(|f| f.registered_at)),
            rdap_updated: date(facts.and_then(|f| f.updated_at)),
            rdap_expires: date(facts.and_then(|f| f.expires_at)),
            rdap_statuses: facts.map(|f| f.statuses.clone()).unwrap_or_default(),
            nameservers: facts.map(|f| f.nameservers.clone()).unwrap_or_default(),
            rdap_url: facts.and_then(|f| safe_link(&f.url)),
            rdap_checked: rdap.map(|c| relative(c.at, now)).unwrap_or_default(),
            rdap_last_ok: rdap
                .and_then(|c| c.last_ok.as_ref())
                .map(|(t, _)| relative(*t, now))
                .unwrap_or_default(),
            rdap_error: rdap.and_then(|c| c.error.clone()).unwrap_or_default(),
            registrar_mismatch: d.registrar_mismatch.clone().unwrap_or_default(),
            missing_from_cloudflare: d.missing_from_cloudflare,
            cloudflare: d
                .cloudflare
                .as_ref()
                .filter(|_| d.declared.registrar == "cloudflare")
                .map(|c| CloudflareRow {
                    expires: date(c.expires_at),
                    auto_renew: yes_no(c.auto_renew),
                    locked: yes_no(c.locked),
                    privacy: yes_no(c.privacy),
                }),
            row,
        }
    }

    /// Whether the expiry shown comes from somewhere other than RDAP and
    /// RDAP disagrees on the date.
    pub fn rdap_disagrees(&self) -> bool {
        !self.rdap_expires.is_empty()
            && self.row.expires_source != Source::Rdap.label()
            && self.rdap_expires != self.row.expires_date
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_links_are_rendered() {
        assert_eq!(
            safe_link("https://dash.cloudflare.com/").as_deref(),
            Some("https://dash.cloudflare.com/")
        );
        assert_eq!(safe_link("javascript:alert(1)"), None);
        assert_eq!(safe_link("not a url"), None);
    }

    #[test]
    fn formats() {
        let now: Timestamp = "2026-09-25T00:00:00Z".parse().unwrap();
        let later: Timestamp = "2026-10-07T00:00:00Z".parse().unwrap();
        assert_eq!(relative(later, now), "in 12d");
        assert_eq!(relative(now, later), "12d ago");
        assert_eq!(fmt_money(10.444, "USD"), "$10.44");
        assert_eq!(fmt_money(16.9, "EUR"), "€16.90");
        assert_eq!(fmt_money(1.0, "CHF"), "1.00 CHF");
    }
}
