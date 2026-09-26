//! Prometheus metrics. Per-domain gauges are computed from the caches on
//! every scrape; counters track RDAP and Cloudflare requests.

use std::sync::Arc;

use jiff::Timestamp;
use prometheus_client::encoding::{DescriptorEncoder, EncodeLabelSet, EncodeMetric};
use prometheus_client::metrics::MetricType;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::ConstGauge;
use prometheus_client::registry::Registry;

use crate::status::{DomainStatus, Domains};

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ResultLabels {
    pub result: String,
}

/// Request counters, shared with the refresher.
#[derive(Default)]
pub struct Counters {
    pub rdap: Family<ResultLabels, Counter>,
    pub cloudflare: Family<ResultLabels, Counter>,
}

impl Counters {
    pub fn rdap(&self, result: &str) {
        self.rdap
            .get_or_create(&ResultLabels {
                result: result.into(),
            })
            .inc();
    }

    pub fn cloudflare(&self, result: &str) {
        self.cloudflare
            .get_or_create(&ResultLabels {
                result: result.into(),
            })
            .inc();
    }
}

pub struct Metrics {
    registry: Registry,
}

impl Metrics {
    pub fn new(domains: Arc<Domains>, counters: &Counters) -> Arc<Self> {
        let mut registry = Registry::default();
        registry.register(
            "dominium_rdap_requests",
            "RDAP lookups by result (ok, not_found, no_server, rate_limited, error)",
            counters.rdap.clone(),
        );
        registry.register(
            "dominium_cloudflare_requests",
            "Cloudflare Registrar listings by result",
            counters.cloudflare.clone(),
        );
        registry.register_collector(Box::new(DomainCollector { domains }));
        Arc::new(Metrics { registry })
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, &self.registry)
            .expect("writing to a String cannot fail");
        out
    }
}

struct DomainCollector {
    domains: Arc<Domains>,
}

impl std::fmt::Debug for DomainCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainCollector").finish_non_exhaustive()
    }
}

type Labels = Vec<(&'static str, String)>;

fn labels(d: &DomainStatus) -> Labels {
    vec![
        ("domain", d.declared.name.clone()),
        ("registrar", d.declared.registrar.clone()),
    ]
}

fn seconds(t: Timestamp) -> f64 {
    t.as_millisecond() as f64 / 1000.0
}

fn bool_value(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

fn gauge(
    encoder: &mut DescriptorEncoder,
    rows: &[DomainStatus],
    name: &str,
    help: &str,
    value: impl Fn(&DomainStatus) -> Option<(Labels, f64)>,
) -> Result<(), std::fmt::Error> {
    let mut metric = encoder.encode_descriptor(name, help, None, MetricType::Gauge)?;
    for row in rows {
        if let Some((labels, v)) = value(row) {
            ConstGauge::new(v).encode(metric.encode_family(&labels)?)?;
        }
    }
    Ok(())
}

impl prometheus_client::collector::Collector for DomainCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let overview = self.domains.overview();
        let rows = &overview.domains;

        {
            let mut info = encoder.encode_descriptor(
                "dominium_domain",
                "Domain metadata, for joins in alert annotations",
                None,
                MetricType::Info,
            )?;
            for d in rows {
                let mut l = labels(d);
                l.push(("purpose", d.declared.purpose.clone().unwrap_or_default()));
                l.push(("source", d.declared.source.clone()));
                info.encode_info(&l)?;
            }
        }
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_expiry_timestamp_seconds",
            "Effective expiry (Cloudflare, else RDAP, else the inventory)",
            |d| d.expires_at.map(|(t, _)| (labels(d), seconds(t))),
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_state_known",
            "1 if some source knows the expiry",
            |d| Some((labels(d), bool_value(d.expires_at.is_some()))),
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_auto_renew",
            "1 if auto-renew is on, 0 if off; absent when unknown",
            |d| d.auto_renews().map(|v| (labels(d), bool_value(v))),
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_renewal_price",
            "Yearly renewal price",
            |d| {
                d.price.as_ref().map(|(p, _)| {
                    let mut l = labels(d);
                    l.push(("currency", p.currency.clone()));
                    (l, p.amount)
                })
            },
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_warn_before_seconds",
            "Warning threshold before expiry",
            |d| Some((labels(d), d.thresholds.warn_before.as_secs_f64())),
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_critical_before_seconds",
            "Critical threshold before expiry",
            |d| Some((labels(d), d.thresholds.critical_before.as_secs_f64())),
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_registrar_mismatch",
            "1 if RDAP reports a different registrar than the inventory",
            |d| {
                let reported = d.rdap.as_ref().and_then(|c| c.value())?.registrar.clone()?;
                let mut l = labels(d);
                l.push(("rdap_registrar", reported));
                Some((l, bool_value(d.registrar_mismatch.is_some())))
            },
        )?;
        gauge(
            &mut encoder,
            rows,
            "dominium_domain_missing_from_cloudflare",
            "1 if declared as a Cloudflare domain but not in the account",
            |d| {
                (d.declared.registrar == "cloudflare" && self.domains.facts.cloudflare_enabled())
                    .then(|| (labels(d), bool_value(d.missing_from_cloudflare)))
            },
        )?;
        {
            let mut checked = encoder.encode_descriptor(
                "dominium_domain_last_checked_timestamp_seconds",
                "Last successful answer per source",
                None,
                MetricType::Gauge,
            )?;
            for d in rows {
                if let Some((at, _)) = d.rdap.as_ref().and_then(|c| c.last_ok.as_ref()) {
                    let mut l = labels(d);
                    l.push(("source", "rdap".into()));
                    ConstGauge::new(seconds(*at)).encode(checked.encode_family(&l)?)?;
                }
            }
        }
        {
            let mut undeclared = encoder.encode_descriptor(
                "dominium_undeclared_domain",
                "A domain in the Cloudflare account that no inventory declares",
                None,
                MetricType::Gauge,
            )?;
            for name in &overview.undeclared {
                let l: Labels = vec![("domain", name.clone())];
                ConstGauge::new(1.0).encode(undeclared.encode_family(&l)?)?;
            }
        }
        {
            let problems = encoder.encode_descriptor(
                "dominium_inventory_problems",
                "Invalid or duplicate inventory entries",
                None,
                MetricType::Gauge,
            )?;
            ConstGauge::new(overview.snapshot.problems.len() as f64).encode(problems)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::CfDomains;
    use crate::facts::FactStore;
    use crate::inventory::{StaticInventory, from_configmaps};
    use crate::rdap::RdapFacts;
    use crate::settings::Settings;
    use crate::status::Thresholds;
    use std::collections::BTreeMap;

    #[test]
    fn exports_domain_gauges() {
        let data = BTreeMap::from([(
            "d.json".to_string(),
            r#"{"version":1,"domains":[
                {"name":"a.com","registrar":"namecheap","purpose":"Blog","expiresAt":"2026-10-01","autoRenew":false,
                 "renewal":{"price":12.5,"currency":"usd"}},
                {"name":"b.dev","registrar":"cloudflare"},
                {"name":"bad","registrar":"x"}]}"#
                .to_string(),
        )]);
        let facts = FactStore::new(true);
        let now = Timestamp::now();
        facts.record_rdap(
            "a.com",
            "2026-09-01T00:00:00Z".parse().unwrap(),
            Ok(RdapFacts {
                registrar: Some("Porkbun LLC".into()),
                ..Default::default()
            }),
        );
        facts.record_cloudflare(
            now,
            Ok(CfDomains::from([(
                "stray.dev".to_string(),
                Default::default(),
            )])),
        );
        let domains = Arc::new(Domains {
            inventory: Arc::new(StaticInventory(from_configmaps([("inv", &data)]))),
            facts,
            settings: Settings::default(),
            thresholds: Thresholds::default(),
        });
        let counters = Counters::default();
        let metrics = Metrics::new(domains, &counters);
        counters.rdap("ok");
        let text = metrics.encode();

        for expected in [
            r#"dominium_domain_expiry_timestamp_seconds{domain="a.com",registrar="namecheap"} 1790812800"#,
            r#"dominium_domain_info{domain="a.com",registrar="namecheap",purpose="Blog",source="inv/d.json"} 1"#,
            r#"dominium_domain_state_known{domain="b.dev",registrar="cloudflare"} 0"#,
            r#"dominium_domain_auto_renew{domain="a.com",registrar="namecheap"} 0"#,
            r#"dominium_domain_renewal_price{domain="a.com",registrar="namecheap",currency="USD"} 12.5"#,
            r#"dominium_domain_warn_before_seconds{domain="a.com",registrar="namecheap"} 2592000"#,
            r#"dominium_domain_registrar_mismatch{domain="a.com",registrar="namecheap",rdap_registrar="Porkbun LLC"} 1"#,
            r#"dominium_domain_missing_from_cloudflare{domain="b.dev",registrar="cloudflare"} 1"#,
            r#"dominium_domain_last_checked_timestamp_seconds{domain="a.com",registrar="namecheap",source="rdap"} 1788220800"#,
            r#"dominium_undeclared_domain{domain="stray.dev"} 1"#,
            "dominium_inventory_problems 1",
            r#"dominium_rdap_requests_total{result="ok"} 1"#,
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
        assert!(!text.contains(r#"dominium_domain_auto_renew{domain="b.dev""#));
    }
}
