//! Cloudflare Registrar: the domains in the account, with expiry, auto-renew
//! and lock state. Needs an account API token with read access to the
//! Registrar and nothing else.
//!
//! Fields are read leniently: anything missing stays unknown.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use url::Url;

pub const API_BASE: &str = "https://api.cloudflare.com/client/v4/";
const MAX_PAGES: u32 = 50;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CfDomain {
    pub expires_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,
    pub auto_renew: Option<bool>,
    pub locked: Option<bool>,
    pub privacy: Option<bool>,
    pub current_registrar: Option<String>,
}

/// Domains in the account, by (lower-case) name.
pub type CfDomains = BTreeMap<String, CfDomain>;

#[async_trait]
pub trait Registrar: Send + Sync {
    async fn list(&self) -> Result<CfDomains, String>;
}

pub struct CloudflareClient {
    http: reqwest::Client,
    base: Url,
    account_id: String,
    token: String,
}

impl std::fmt::Debug for CloudflareClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudflareClient")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

impl CloudflareClient {
    pub fn new(base: Url, account_id: String, token: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("dominium/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        let base = if base.as_str().ends_with('/') {
            base
        } else {
            format!("{base}/").parse()?
        };
        Ok(CloudflareClient {
            http,
            base,
            account_id,
            token: token.trim().to_string(),
        })
    }

    async fn page(&self, page: u32) -> Result<serde_json::Value, String> {
        let mut url = self
            .base
            .join(&format!("accounts/{}/registrar/domains", self.account_id))
            .map_err(|e| e.to_string())?;
        url.query_pairs_mut()
            .append_pair("page", &page.to_string())
            .append_pair("per_page", "50");
        let res = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("Cloudflare request failed: {e}"))?;
        let status = res.status();
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|e| format!("Cloudflare answered {status} with an unreadable body: {e}"))?;
        if !status.is_success() || body.get("success").and_then(|s| s.as_bool()) == Some(false) {
            let errors = body
                .get("errors")
                .and_then(|e| e.as_array())
                .map(|errs| {
                    errs.iter()
                        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            return Err(format!("Cloudflare answered {status}: {errors}"));
        }
        Ok(body)
    }
}

fn ts(v: &serde_json::Value, key: &str) -> Option<Timestamp> {
    v.get(key)?.as_str()?.parse().ok()
}

fn flag(v: &serde_json::Value, key: &str) -> Option<bool> {
    v.get(key)?.as_bool()
}

/// Parses the `result` array of one page.
pub fn parse_page(body: &serde_json::Value) -> CfDomains {
    body.get("result")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let name = d
                .get("name")
                .or_else(|| d.get("id"))
                .and_then(|n| n.as_str())?
                .trim_end_matches('.')
                .to_ascii_lowercase();
            Some((
                name,
                CfDomain {
                    expires_at: ts(d, "expires_at"),
                    created_at: ts(d, "created_at"),
                    auto_renew: flag(d, "auto_renew"),
                    locked: flag(d, "locked"),
                    privacy: flag(d, "privacy"),
                    current_registrar: d
                        .get("current_registrar")
                        .and_then(|r| r.as_str())
                        .map(str::to_string),
                },
            ))
        })
        .collect()
}

#[async_trait]
impl Registrar for CloudflareClient {
    async fn list(&self) -> Result<CfDomains, String> {
        let mut all = CfDomains::new();
        let mut page = 1;
        loop {
            let body = self.page(page).await?;
            all.extend(parse_page(&body));
            let total_pages = body
                .pointer("/result_info/total_pages")
                .and_then(|t| t.as_u64())
                .unwrap_or(1);
            if u64::from(page) >= total_pages || page >= MAX_PAGES {
                return Ok(all);
            }
            page += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn lists_all_pages() {
        let server = MockServer::start().await;
        let page = |n: u32, names: &[&str]| {
            json!({
                "success": true, "errors": [],
                "result": names.iter().map(|n| json!({
                    "name": n, "expires_at": "2027-03-01T10:00:00Z", "auto_renew": true,
                    "locked": true, "current_registrar": "Cloudflare"
                })).collect::<Vec<_>>(),
                "result_info": {"page": n, "total_pages": 2}
            })
        };
        for (n, names) in [(1, vec!["a.com"]), (2, vec!["B.dev"])] {
            Mock::given(method("GET"))
                .and(path("/accounts/acc/registrar/domains"))
                .and(query_param("page", n.to_string()))
                .and(header("authorization", "Bearer tok"))
                .respond_with(ResponseTemplate::new(200).set_body_json(page(n, &names)))
                .mount(&server)
                .await;
        }
        let client =
            CloudflareClient::new(server.uri().parse().unwrap(), "acc".into(), "tok\n".into())
                .unwrap();
        let domains = client.list().await.unwrap();
        assert_eq!(domains.keys().collect::<Vec<_>>(), vec!["a.com", "b.dev"]);
        let a = &domains["a.com"];
        assert_eq!(a.auto_renew, Some(true));
        assert_eq!(a.expires_at, Some("2027-03-01T10:00:00Z".parse().unwrap()));
        assert_eq!(a.privacy, None);
    }

    #[tokio::test]
    async fn reports_api_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "success": false, "errors": [{"code": 9109, "message": "Unauthorized to access requested resource"}]
            })))
            .mount(&server)
            .await;
        let client =
            CloudflareClient::new(server.uri().parse().unwrap(), "acc".into(), "t".into()).unwrap();
        let err = client.list().await.unwrap_err();
        assert!(err.contains("403") && err.contains("Unauthorized"), "{err}");
    }
}
