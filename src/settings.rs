//! The settings file: registrars (display name, dashboard link, how RDAP
//! names them, renewal prices per TLD) and RDAP server overrides.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

use crate::inventory::{Declared, Price};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registrar {
    /// Shown in the UI; defaults to the key.
    #[serde(default)]
    pub name: Option<String>,
    /// Where domains are managed at this registrar.
    #[serde(default)]
    pub url: Option<String>,
    /// Substrings of the registrar name RDAP reports (compared lower-case,
    /// ignoring punctuation). Defaults to the key.
    #[serde(default)]
    pub rdap_names: Vec<String>,
    /// Currency of `prices`.
    #[serde(default)]
    pub currency: Option<String>,
    /// Yearly renewal price per TLD (`com`, `dev`, `pt`, …).
    #[serde(default, deserialize_with = "prices")]
    pub prices: BTreeMap<String, f64>,
}

/// Prices as numbers or numeric strings (`helm --set` produces strings).
fn prices<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Amount {
        Number(f64),
        Text(String),
    }
    BTreeMap::<String, Amount>::deserialize(d)?
        .into_iter()
        .map(|(tld, amount)| {
            let value = match amount {
                Amount::Number(n) => Some(n),
                Amount::Text(t) => t.trim().parse().ok(),
            }
            .filter(|v: &f64| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid price for .{tld}")))?;
            Ok((tld.trim_start_matches('.').to_ascii_lowercase(), value))
        })
        .collect()
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RdapSettings {
    /// RDAP base URL per TLD, for TLDs missing from (or wrong in) the IANA
    /// bootstrap file.
    #[serde(default)]
    pub servers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub registrars: BTreeMap<String, Registrar>,
    #[serde(default)]
    pub rdap: RdapSettings,
}

fn builtin(name: &str, url: &str) -> Registrar {
    Registrar {
        name: Some(name.into()),
        url: Some(url.into()),
        ..Default::default()
    }
}

impl Default for Settings {
    /// The registrars in use, without prices.
    fn default() -> Self {
        let registrars = BTreeMap::from([
            (
                "cloudflare".to_string(),
                builtin(
                    "Cloudflare",
                    "https://dash.cloudflare.com/?to=/:account/domains",
                ),
            ),
            (
                "namecheap".to_string(),
                builtin("Namecheap", "https://ap.www.namecheap.com/domains/list/"),
            ),
            (
                "porkbun".to_string(),
                builtin("Porkbun", "https://porkbun.com/account/domainsSpeedy"),
            ),
            (
                "ptisp".to_string(),
                builtin("PTisp", "https://www.ptisp.pt/"),
            ),
        ]);
        Settings {
            registrars,
            rdap: RdapSettings::default(),
        }
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl Settings {
    /// Reads the settings file. A registrar in the file is merged into the
    /// built-in entry with the same key: fields it sets win, prices are added.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Settings> {
        let mut settings = Settings::default();
        let Some(path) = path else {
            return Ok(settings);
        };
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: Settings =
            serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        for (key, r) in file.registrars {
            let entry = settings.registrars.entry(key).or_default();
            entry.name = r.name.or(entry.name.take());
            entry.url = r.url.or(entry.url.take());
            entry.currency = r.currency.or(entry.currency.take());
            if !r.rdap_names.is_empty() {
                entry.rdap_names = r.rdap_names;
            }
            entry.prices.extend(r.prices);
        }
        settings.rdap = file.rdap;
        Ok(settings)
    }

    pub fn registrar_name(&self, key: &str) -> String {
        self.registrars
            .get(key)
            .and_then(|r| r.name.clone())
            .unwrap_or_else(|| key.to_string())
    }

    pub fn registrar_url(&self, key: &str) -> Option<String> {
        self.registrars.get(key).and_then(|r| r.url.clone())
    }

    /// Whether the registrar name reported by RDAP is the declared registrar.
    pub fn rdap_matches(&self, key: &str, rdap_registrar: &str) -> bool {
        let reported = squash(rdap_registrar);
        let configured = self
            .registrars
            .get(key)
            .map(|r| r.rdap_names.as_slice())
            .unwrap_or_default();
        let mut candidates: Vec<String> = configured.iter().map(|n| squash(n)).collect();
        if candidates.is_empty() {
            candidates.push(squash(key));
        }
        candidates
            .iter()
            .any(|c| !c.is_empty() && reported.contains(c.as_str()))
    }

    /// Price from the registrar's table for the domain's TLD.
    pub fn table_price(&self, domain: &Declared) -> Option<Price> {
        let r = self.registrars.get(&domain.registrar)?;
        let amount = *r.prices.get(domain.tld())?;
        Some(Price {
            amount,
            currency: r
                .currency
                .clone()
                .unwrap_or_else(|| "USD".into())
                .to_ascii_uppercase(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(name: &str, registrar: &str) -> Declared {
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
            source: "test".into(),
        }
    }

    #[test]
    fn file_overrides_builtins_per_key() {
        let dir = std::env::temp_dir().join(format!("dominium-settings-{}", std::process::id()));
        std::fs::write(
            &dir,
            "registrars:\n  cloudflare:\n    name: CF\n    currency: usd\n    prices: { com: 10.44 }\n  gandi: {}\nrdap:\n  servers: { pt: https://rdap.example.pt/ }\n",
        )
        .unwrap();
        let s = Settings::load(Some(&dir)).unwrap();
        std::fs::remove_file(&dir).ok();
        assert_eq!(s.registrar_name("cloudflare"), "CF");
        assert_eq!(
            s.registrar_url("cloudflare").as_deref(),
            Some("https://dash.cloudflare.com/?to=/:account/domains")
        );
        assert_eq!(s.registrar_name("namecheap"), "Namecheap");
        assert_eq!(s.registrar_name("gandi"), "gandi");
        assert_eq!(s.registrar_name("unknown"), "unknown");
        assert_eq!(s.rdap.servers["pt"], "https://rdap.example.pt/");
        assert_eq!(
            s.table_price(&domain("a.com", "cloudflare")),
            Some(Price {
                amount: 10.44,
                currency: "USD".into()
            })
        );
        assert_eq!(s.table_price(&domain("a.dev", "cloudflare")), None);
    }

    #[test]
    fn rdap_registrar_matching() {
        let mut s = Settings::default();
        assert!(s.rdap_matches("cloudflare", "Cloudflare, Inc."));
        assert!(s.rdap_matches("namecheap", "NameCheap, Inc."));
        assert!(s.rdap_matches("porkbun", "Porkbun LLC"));
        assert!(!s.rdap_matches("cloudflare", "NameCheap, Inc."));
        s.registrars.get_mut("ptisp").unwrap().rdap_names =
            vec!["PT ISP".into(), "Dominios PT".into()];
        assert!(s.rdap_matches("ptisp", "PTISP - Solucoes"));
        assert!(s.rdap_matches("ptisp", "DOMINIOS.PT, Lda"));
        assert!(!s.rdap_matches("ptisp", "ptisp-other".replace("ptisp", "x").as_str()));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(serde_yaml::from_str::<Settings>("registrars: { a: { price: 1 } }").is_err());
        assert!(
            serde_yaml::from_str::<Settings>("registrars: { a: { prices: { com: x } } }").is_err()
        );
        let s: Settings =
            serde_yaml::from_str("registrars: { a: { prices: { com: \"10.44\", .DEV: 12 } } }")
                .unwrap();
        assert_eq!(s.registrars["a"].prices["com"], 10.44);
        assert_eq!(s.registrars["a"].prices["dev"], 12.0);
    }
}
