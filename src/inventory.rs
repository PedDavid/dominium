//! The domain inventory: which domains exist, as declared in labelled
//! ConfigMaps (written by the Pulumi program that manages the domains).

use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Deserialize;

use crate::duration;

/// Label selecting inventory ConfigMaps.
pub const INVENTORY_LABEL: &str = "dominium.prdv.cloud/inventory";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    pub amount: f64,
    pub currency: String,
}

/// A domain as declared in the inventory.
#[derive(Clone, Debug, PartialEq)]
pub struct Declared {
    pub name: String,
    pub registrar: String,
    pub purpose: Option<String>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    pub auto_renew: Option<bool>,
    pub renewal: Option<Price>,
    pub expires_at: Option<Timestamp>,
    pub warn_before: Option<jiff::SignedDuration>,
    pub critical_before: Option<jiff::SignedDuration>,
    /// `<configmap>/<key>` the domain came from.
    pub source: String,
}

impl Declared {
    /// The last label, e.g. `cloud` for `prdv.cloud`.
    pub fn tld(&self) -> &str {
        self.name.rsplit('.').next().unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub source: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Sorted by name, unique.
    pub domains: Vec<Declared>,
    pub problems: Vec<Problem>,
}

impl Snapshot {
    pub fn get(&self, name: &str) -> Option<&Declared> {
        self.domains.iter().find(|d| d.name == name)
    }
}

/// Where the inventory comes from: ConfigMaps in the cluster, or a fixed
/// snapshot (demo mode and tests).
pub trait Inventory: Send + Sync {
    fn snapshot(&self) -> Snapshot;
    fn ready(&self) -> bool;
}

pub struct StaticInventory(pub Snapshot);

impl Inventory for StaticInventory {
    fn snapshot(&self) -> Snapshot {
        self.0.clone()
    }
    fn ready(&self) -> bool {
        true
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    domains: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    name: String,
    registrar: String,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    auto_renew: Option<bool>,
    #[serde(default)]
    renewal: Option<RenewalEntry>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    alerts: Option<AlertsEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenewalEntry {
    price: f64,
    currency: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AlertsEntry {
    #[serde(default)]
    warn_before: Option<String>,
    #[serde(default)]
    critical_before: Option<String>,
}

/// Lower-cases and strips a trailing dot; `None` if it isn't a plausible
/// (ASCII or punycode) domain name with at least two labels.
pub fn normalize_domain(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();
    let valid_label = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    (labels.len() >= 2 && name.len() <= 253 && labels.iter().all(valid_label)).then_some(name)
}

/// Parses a date (`2027-03-01`) as midnight UTC, or a full RFC 3339 timestamp.
pub fn parse_when(value: &str) -> Option<Timestamp> {
    let value = value.trim();
    if let Ok(ts) = value.parse::<Timestamp>() {
        return Some(ts);
    }
    let date: Date = value.parse().ok()?;
    date.to_zoned(TimeZone::UTC).ok().map(|z| z.timestamp())
}

fn convert(entry: Entry, source: &str) -> Result<Declared, String> {
    let name =
        normalize_domain(&entry.name).ok_or_else(|| format!("invalid domain {:?}", entry.name))?;
    let registrar = entry.registrar.trim().to_ascii_lowercase();
    if registrar.is_empty()
        || !registrar
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(format!("{name}: invalid registrar {:?}", entry.registrar));
    }
    let expires_at = match entry.expires_at.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(v) => Some(parse_when(v).ok_or_else(|| format!("{name}: invalid expiresAt {v:?}"))?),
    };
    let renewal = match entry.renewal {
        None => None,
        Some(r) if r.price.is_finite() && r.price >= 0.0 && !r.currency.trim().is_empty() => {
            Some(Price {
                amount: r.price,
                currency: r.currency.trim().to_ascii_uppercase(),
            })
        }
        Some(_) => return Err(format!("{name}: renewal needs a price >= 0 and a currency")),
    };
    let alerts = entry.alerts.unwrap_or(AlertsEntry {
        warn_before: None,
        critical_before: None,
    });
    let parse_duration = |v: Option<String>| -> Result<_, String> {
        v.map(|d| duration::parse(&d).map_err(|e| format!("{name}: {e}")))
            .transpose()
    };
    let non_empty = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Ok(Declared {
        registrar,
        purpose: non_empty(entry.purpose),
        tags: entry
            .tags
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        notes: non_empty(entry.notes),
        auto_renew: entry.auto_renew,
        renewal,
        expires_at,
        warn_before: parse_duration(alerts.warn_before)?,
        critical_before: parse_duration(alerts.critical_before)?,
        source: source.to_string(),
        name,
    })
}

/// Parses one inventory file. Entries that fail are reported and skipped;
/// a file that can't be parsed at all is a single problem.
pub fn parse_file(source: &str, content: &str) -> (Vec<Declared>, Vec<Problem>) {
    let problem = |message: String| Problem {
        source: source.to_string(),
        message,
    };
    let file: File = match serde_json::from_str(content) {
        Ok(f) => f,
        Err(e) => return (vec![], vec![problem(format!("invalid JSON: {e}"))]),
    };
    if file.version != FORMAT_VERSION {
        return (
            vec![],
            vec![problem(format!(
                "unsupported version {} (expected {FORMAT_VERSION})",
                file.version
            ))],
        );
    }
    let mut domains = Vec::new();
    let mut problems = Vec::new();
    for entry in file.domains {
        match convert(entry, source) {
            Ok(d) => domains.push(d),
            Err(e) => problems.push(problem(e)),
        }
    }
    (domains, problems)
}

/// Builds a snapshot from ConfigMaps: `(configmap name, data)`. Every key
/// ending in `.json` is an inventory file. A domain declared more than once
/// is kept from the first source (by name) and reported.
pub fn from_configmaps<'a>(
    maps: impl IntoIterator<Item = (&'a str, &'a BTreeMap<String, String>)>,
) -> Snapshot {
    let mut files: Vec<(String, &String)> = maps
        .into_iter()
        .flat_map(|(cm, data)| {
            data.iter()
                .filter(|(k, _)| k.ends_with(".json"))
                .map(move |(k, v)| (format!("{cm}/{k}"), v))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut by_name: HashMap<String, Declared> = HashMap::new();
    let mut problems = Vec::new();
    for (source, content) in files {
        let (domains, mut file_problems) = parse_file(&source, content);
        problems.append(&mut file_problems);
        for d in domains {
            if let Some(first) = by_name.get(&d.name) {
                problems.push(Problem {
                    source: source.clone(),
                    message: format!(
                        "{} is also declared in {}; ignored here",
                        d.name, first.source
                    ),
                });
            } else {
                by_name.insert(d.name.clone(), d);
            }
        }
    }
    let mut domains: Vec<Declared> = by_name.into_values().collect();
    domains.sort_by(|a, b| a.name.cmp(&b.name));
    Snapshot { domains, problems }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_a_full_entry() {
        let (domains, problems) = parse_file(
            "domains/domains.json",
            r#"{"version":1,"domains":[{
                "name":"Example.COM.","registrar":"Cloudflare","purpose":"Blog",
                "tags":["web"," "],"notes":"n","autoRenew":true,
                "renewal":{"price":10.44,"currency":"usd"},
                "expiresAt":"2027-03-01","alerts":{"warnBefore":"60d"}}]}"#,
        );
        assert!(problems.is_empty(), "{problems:?}");
        let d = &domains[0];
        assert_eq!(d.name, "example.com");
        assert_eq!(d.registrar, "cloudflare");
        assert_eq!(d.tld(), "com");
        assert_eq!(d.tags, vec!["web"]);
        assert_eq!(d.auto_renew, Some(true));
        assert_eq!(
            d.renewal,
            Some(Price {
                amount: 10.44,
                currency: "USD".into()
            })
        );
        assert_eq!(d.expires_at, Some("2027-03-01T00:00:00Z".parse().unwrap()));
        assert_eq!(
            d.warn_before,
            Some(jiff::SignedDuration::from_hours(60 * 24))
        );
        assert_eq!(d.critical_before, None);
    }

    #[test]
    fn bad_entries_are_reported_not_fatal() {
        let (domains, problems) = parse_file(
            "cm/a.json",
            r#"{"version":1,"domains":[
                {"name":"ok.pt","registrar":"ptisp"},
                {"name":"not a domain","registrar":"x"},
                {"name":"b.com","registrar":"porkbun","expiresAt":"soon"},
                {"name":"c.com","registrar":"porkbun","alerts":{"warnBefore":"1y"}}]}"#,
        );
        assert_eq!(domains.len(), 1);
        assert_eq!(problems.len(), 3);
        assert!(problems[0].message.contains("invalid domain"));
    }

    #[test]
    fn unknown_fields_and_versions_are_rejected() {
        let (_, p) = parse_file("x", r#"{"version":2,"domains":[]}"#);
        assert!(p[0].message.contains("unsupported version"));
        let (_, p) = parse_file(
            "x",
            r#"{"version":1,"domains":[{"name":"a.com","registrar":"x","typo":1}]}"#,
        );
        assert!(p[0].message.contains("typo"), "{p:?}");
        let (_, p) = parse_file("x", "not json");
        assert!(p[0].message.starts_with("invalid JSON"));
    }

    #[test]
    fn merges_configmaps_and_reports_duplicates() {
        let a = data(&[
            (
                "domains.json",
                r#"{"version":1,"domains":[{"name":"a.com","registrar":"cloudflare"}]}"#,
            ),
            ("README", "ignored"),
        ]);
        let b = data(&[(
            "domains.json",
            r#"{"version":1,"domains":[{"name":"A.com","registrar":"namecheap"},{"name":"b.dev","registrar":"porkbun"}]}"#,
        )]);
        let snap = from_configmaps([("stack-b", &b), ("stack-a", &a)]);
        let names: Vec<_> = snap.domains.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["a.com", "b.dev"]);
        assert_eq!(snap.get("a.com").unwrap().registrar, "cloudflare");
        assert_eq!(snap.problems.len(), 1);
        assert_eq!(snap.problems[0].source, "stack-b/domains.json");
        assert!(snap.problems[0].message.contains("stack-a/domains.json"));
    }

    #[test]
    fn domain_names() {
        assert_eq!(
            normalize_domain("xn--bcher-kva.de").as_deref(),
            Some("xn--bcher-kva.de")
        );
        for bad in ["com", "a..com", "-a.com", "a_b.com", "", "a.com/x"] {
            assert_eq!(normalize_domain(bad), None, "{bad}");
        }
    }
}
