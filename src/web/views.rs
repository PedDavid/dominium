//! View models: domain statuses turned into pre-formatted rows for the templates.

use jiff::{SignedDuration, Timestamp};

use crate::duration::humanize;
use crate::fx::Estimate;
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

/// Yearly renewals as one amount in the viewer's currency, with the
/// per-currency breakdown and the rates used.
pub struct EstimateView {
    pub currency: String,
    pub total: String,
    /// Some amount was converted, so the total is an estimate.
    pub approximate: bool,
    /// Some currency had no rate and is left out of the total.
    pub incomplete: bool,
    pub lines: Vec<EstimateLine>,
}

pub struct EstimateLine {
    pub original: String,
    /// Empty when no conversion was needed or possible.
    pub converted: String,
    /// "1 USD = 0.9249 EUR · ECB reference rate of 2026-10-09", or why not.
    pub rate: String,
}

impl EstimateView {
    pub fn new(e: &Estimate) -> Option<EstimateView> {
        if e.parts.is_empty() {
            return None;
        }
        let lines = e
            .parts
            .iter()
            .map(|p| EstimateLine {
                original: fmt_money(p.amount, &p.currency),
                converted: match (p.currency == e.currency, p.converted) {
                    (false, Some(c)) => fmt_money(c, &e.currency),
                    _ => String::new(),
                },
                rate: match p.rate {
                    _ if p.currency == e.currency => String::new(),
                    Some(r) => format!(
                        "1 {} = {} {} · {}",
                        p.currency,
                        fmt_rate(r),
                        e.currency,
                        p.origin
                    ),
                    None => format!("no {} rate: left out of the total", p.currency),
                },
            })
            .collect();
        Some(EstimateView {
            currency: e.currency.clone(),
            total: fmt_money(e.total, &e.currency),
            approximate: e.is_estimate(),
            incomplete: e.missing_rates(),
            lines,
        })
    }
}

/// Four significant-looking decimals for small rates, fewer for large ones.
pub fn fmt_rate(r: f64) -> String {
    if r >= 100.0 {
        format!("{r:.2}")
    } else {
        format!("{r:.4}")
    }
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
        assert_eq!(fmt_rate(0.924_855), "0.9249");
        assert_eq!(fmt_rate(162.1), "162.10");
    }

    #[test]
    fn estimate_lines() {
        use crate::fx::{FetchedRates, RateTable};
        let table = RateTable::new(
            Some(&FetchedRates {
                date: Some("2026-10-09".parse().unwrap()),
                per_eur: [("USD".to_string(), 1.08)].into(),
            }),
            &Default::default(),
        );
        let prices = [
            Price {
                amount: 108.0,
                currency: "USD".into(),
            },
            Price {
                amount: 16.9,
                currency: "EUR".into(),
            },
            Price {
                amount: 5.0,
                currency: "BRL".into(),
            },
        ];
        let v = EstimateView::new(&Estimate::compute(&prices, &table, "EUR")).unwrap();
        assert_eq!(v.total, "€116.90");
        assert!(v.approximate && v.incomplete);
        assert_eq!(v.lines[0].original, "$108.00");
        assert_eq!(v.lines[0].converted, "€100.00");
        assert_eq!(
            v.lines[0].rate,
            "1 USD = 0.9259 EUR · ECB reference rate of 2026-10-09"
        );
        assert_eq!(v.lines[1].converted, "");
        assert_eq!(v.lines[1].rate, "");
        assert_eq!(v.lines[2].rate, "no BRL rate: left out of the total");
        assert!(EstimateView::new(&Estimate::compute(&[], &table, "EUR")).is_none());
    }
}
