//! RDAP lookups: expiry, registrar, statuses and nameservers of a domain,
//! from the registry's public RDAP server (found through the IANA bootstrap
//! file, or overridden per TLD in the settings).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;

pub const IANA_BOOTSTRAP: &str = "https://data.iana.org/rdap/dns.json";
const BOOTSTRAP_MAX_AGE: SignedDuration = SignedDuration::from_hours(24);
const BOOTSTRAP_RETRY: SignedDuration = SignedDuration::from_mins(10);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RdapFacts {
    pub expires_at: Option<Timestamp>,
    pub registered_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub registrar: Option<String>,
    pub statuses: Vec<String>,
    pub nameservers: Vec<String>,
    /// The URL that answered.
    pub url: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RdapError {
    #[error(
        "no RDAP server known for .{0}; set expiresAt in the inventory or an RDAP server in the settings"
    )]
    NoServer(String),
    #[error("the registry has no record of this domain")]
    NotFound,
    #[error("rate limited by the RDAP server")]
    RateLimited,
    #[error("{0}")]
    Other(String),
}

impl RdapError {
    pub fn label(&self) -> &'static str {
        match self {
            RdapError::NoServer(_) => "no_server",
            RdapError::NotFound => "not_found",
            RdapError::RateLimited => "rate_limited",
            RdapError::Other(_) => "error",
        }
    }
}

#[async_trait]
pub trait RdapLookup: Send + Sync {
    async fn lookup(&self, domain: &str) -> Result<RdapFacts, RdapError>;
}

struct Bootstrap {
    /// TLD → base URL.
    servers: HashMap<String, String>,
    fetched_at: Option<Timestamp>,
    attempted_at: Option<Timestamp>,
}

pub struct RdapClient {
    http: reqwest::Client,
    bootstrap_url: Url,
    overrides: BTreeMap<String, String>,
    bootstrap: Mutex<Bootstrap>,
}

#[derive(Deserialize)]
struct BootstrapFile {
    services: Vec<(Vec<String>, Vec<String>)>,
}

fn servers_from(file: BootstrapFile) -> HashMap<String, String> {
    let mut servers = HashMap::new();
    for (tlds, urls) in file.services {
        // Prefer https when a service lists several URLs.
        let Some(url) = urls
            .iter()
            .find(|u| u.starts_with("https://"))
            .or(urls.first())
        else {
            continue;
        };
        for tld in tlds {
            servers.insert(tld.to_ascii_lowercase(), url.clone());
        }
    }
    servers
}

impl RdapClient {
    pub fn new(bootstrap_url: Url, overrides: BTreeMap<String, String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("dominium/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(RdapClient {
            http,
            bootstrap_url,
            overrides: overrides
                .into_iter()
                .map(|(k, v)| (k.trim_start_matches('.').to_ascii_lowercase(), v))
                .collect(),
            bootstrap: Mutex::new(Bootstrap {
                servers: HashMap::new(),
                fetched_at: None,
                attempted_at: None,
            }),
        })
    }

    async fn fetch_bootstrap(&self) -> Result<HashMap<String, String>, String> {
        let file: BootstrapFile = self
            .http
            .get(self.bootstrap_url.clone())
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("fetching the RDAP bootstrap file: {e}"))?
            .json()
            .await
            .map_err(|e| format!("parsing the RDAP bootstrap file: {e}"))?;
        Ok(servers_from(file))
    }

    async fn server_for(&self, tld: &str) -> Result<String, RdapError> {
        if let Some(url) = self.overrides.get(tld) {
            return Ok(url.clone());
        }
        let mut boot = self.bootstrap.lock().await;
        let now = Timestamp::now();
        let stale = boot
            .fetched_at
            .is_none_or(|t| now.duration_since(t) > BOOTSTRAP_MAX_AGE);
        let may_retry = boot
            .attempted_at
            .is_none_or(|t| now.duration_since(t) > BOOTSTRAP_RETRY);
        if stale && may_retry {
            boot.attempted_at = Some(now);
            match self.fetch_bootstrap().await {
                Ok(servers) => {
                    tracing::info!(tlds = servers.len(), "RDAP bootstrap loaded");
                    boot.servers = servers;
                    boot.fetched_at = Some(now);
                }
                // Keep using the previous copy, if any.
                Err(e) if boot.fetched_at.is_some() => {
                    tracing::warn!(error = %e, "RDAP bootstrap refresh failed")
                }
                Err(e) => return Err(RdapError::Other(e)),
            }
        }
        if boot.fetched_at.is_none() {
            return Err(RdapError::Other(
                "the RDAP bootstrap file is not loaded yet".into(),
            ));
        }
        boot.servers
            .get(tld)
            .cloned()
            .ok_or_else(|| RdapError::NoServer(tld.to_string()))
    }
}

#[async_trait]
impl RdapLookup for RdapClient {
    async fn lookup(&self, domain: &str) -> Result<RdapFacts, RdapError> {
        let tld = domain.rsplit('.').next().unwrap_or(domain);
        let base = self.server_for(tld).await?;
        let base = if base.ends_with('/') {
            base
        } else {
            format!("{base}/")
        };
        let url = Url::parse(&base)
            .and_then(|b| b.join(&format!("domain/{domain}")))
            .map_err(|e| RdapError::Other(format!("invalid RDAP server {base:?}: {e}")))?;
        let res = self
            .http
            .get(url.clone())
            .header("accept", "application/rdap+json, application/json")
            .send()
            .await
            .map_err(|e| {
                RdapError::Other(format!(
                    "RDAP request to {}: {e}",
                    url.host_str().unwrap_or("?")
                ))
            })?;
        match res.status().as_u16() {
            200 => {}
            404 => return Err(RdapError::NotFound),
            429 => return Err(RdapError::RateLimited),
            s => return Err(RdapError::Other(format!("RDAP server answered {s}"))),
        }
        let final_url = res.url().to_string();
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|e| RdapError::Other(format!("invalid RDAP response: {e}")))?;
        let mut facts = parse(&body);
        facts.url = final_url;
        Ok(facts)
    }
}

fn event(body: &serde_json::Value, action: &str) -> Option<Timestamp> {
    body.get("events")?
        .as_array()?
        .iter()
        .find(|e| e.get("eventAction").and_then(|a| a.as_str()) == Some(action))?
        .get("eventDate")?
        .as_str()?
        .parse()
        .ok()
}

/// The `fn` of an entity's jCard, else its handle.
fn entity_name(entity: &serde_json::Value) -> Option<String> {
    let from_vcard = entity
        .get("vcardArray")
        .and_then(|v| v.get(1))
        .and_then(|props| props.as_array())
        .and_then(|props| {
            props.iter().find_map(|p| {
                let p = p.as_array()?;
                (p.first()?.as_str()? == "fn").then(|| p.get(3)?.as_str().map(str::to_string))?
            })
        })
        .filter(|s| !s.trim().is_empty());
    from_vcard.or_else(|| {
        entity
            .get("handle")
            .and_then(|h| h.as_str())
            .map(str::to_string)
    })
}

/// Extracts the facts from an RDAP domain object. Missing parts stay empty.
pub fn parse(body: &serde_json::Value) -> RdapFacts {
    let registrar = body
        .get("entities")
        .and_then(|e| e.as_array())
        .and_then(|entities| {
            entities.iter().find(|e| {
                e.get("roles")
                    .and_then(|r| r.as_array())
                    .is_some_and(|r| r.iter().any(|r| r.as_str() == Some("registrar")))
            })
        })
        .and_then(entity_name);
    let strings = |key: &str| -> Vec<String> {
        body.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut nameservers: Vec<String> = body
        .get("nameservers")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|ns| ns.get("ldhName").and_then(|n| n.as_str()))
                .map(|n| n.trim_end_matches('.').to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    nameservers.sort();
    RdapFacts {
        expires_at: event(body, "expiration"),
        registered_at: event(body, "registration"),
        updated_at: event(body, "last changed"),
        registrar,
        statuses: strings("status"),
        nameservers,
        url: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample() -> serde_json::Value {
        json!({
            "objectClassName": "domain",
            "ldhName": "EXAMPLE.COM",
            "status": ["client transfer prohibited"],
            "events": [
                {"eventAction": "registration", "eventDate": "2019-03-01T10:00:00Z"},
                {"eventAction": "expiration", "eventDate": "2027-03-01T10:00:00Z"},
                {"eventAction": "last changed", "eventDate": "2026-02-20T08:30:00.123Z"},
                {"eventAction": "last update of RDAP database", "eventDate": "2026-09-26T00:00:00Z"}
            ],
            "entities": [
                {"roles": ["abuse"], "handle": "x"},
                {"roles": ["registrar"], "handle": "1910",
                 "vcardArray": ["vcard", [["version", {}, "text", "4.0"], ["fn", {}, "text", "Cloudflare, Inc."]]]}
            ],
            "nameservers": [{"ldhName": "NOLA.NS.CLOUDFLARE.COM"}, {"ldhName": "ada.ns.cloudflare.com."}]
        })
    }

    #[test]
    fn parses_domain_object() {
        let f = parse(&sample());
        assert_eq!(f.expires_at, Some("2027-03-01T10:00:00Z".parse().unwrap()));
        assert_eq!(
            f.registered_at,
            Some("2019-03-01T10:00:00Z".parse().unwrap())
        );
        assert!(f.updated_at.is_some());
        assert_eq!(f.registrar.as_deref(), Some("Cloudflare, Inc."));
        assert_eq!(f.statuses, vec!["client transfer prohibited"]);
        assert_eq!(
            f.nameservers,
            vec!["ada.ns.cloudflare.com", "nola.ns.cloudflare.com"]
        );
    }

    #[test]
    fn registrar_falls_back_to_handle_and_missing_parts_are_empty() {
        let f = parse(&json!({"entities": [{"roles": ["registrar"], "handle": "PTISP"}]}));
        assert_eq!(f.registrar.as_deref(), Some("PTISP"));
        assert_eq!(f.expires_at, None);
        assert!(f.nameservers.is_empty());
    }

    #[tokio::test]
    async fn looks_up_through_bootstrap_and_overrides() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/dns.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "version": "1.0",
                "services": [[["com", "net"], [format!("{base}/com/")]]]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/com/domain/example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/com/domain/gone.com"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/pt/domain/exemplo.pt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [{"eventAction": "expiration", "eventDate": "2027-01-01T00:00:00+00:00"}]
            })))
            .mount(&server)
            .await;

        let client = RdapClient::new(
            format!("{base}/dns.json").parse().unwrap(),
            BTreeMap::from([(".PT".into(), format!("{base}/pt"))]),
        )
        .unwrap();
        let f = client.lookup("example.com").await.unwrap();
        assert_eq!(f.registrar.as_deref(), Some("Cloudflare, Inc."));
        assert!(f.url.ends_with("/com/domain/example.com"));
        assert_eq!(client.lookup("gone.com").await, Err(RdapError::NotFound));
        let pt = client.lookup("exemplo.pt").await.unwrap();
        assert_eq!(pt.expires_at, Some("2027-01-01T00:00:00Z".parse().unwrap()));
        assert_eq!(
            client.lookup("a.zz").await,
            Err(RdapError::NoServer("zz".into()))
        );
    }

    #[test]
    fn bootstrap_prefers_https() {
        let file: BootstrapFile = serde_json::from_value(json!({
            "services": [[["com"], ["http://a/", "https://b/"]], [["dev"], ["http://c/"]], [["x"], []]]
        }))
        .unwrap();
        let servers = servers_from(file);
        assert_eq!(servers["com"], "https://b/");
        assert_eq!(servers["dev"], "http://c/");
        assert!(!servers.contains_key("x"));
    }
}
