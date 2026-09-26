//! Command-line flags, each also settable through a `DOMINIUM_*` environment variable.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use jiff::SignedDuration;
use url::Url;

use crate::duration;
use crate::facts::Intervals;
use crate::status::Thresholds;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "dominium",
    version,
    about = "Inventory and expiry alerts for domains"
)]
pub struct Config {
    /// Address for the UI.
    #[arg(long, env = "DOMINIUM_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,

    /// Address for /metrics, /healthz and /readyz.
    #[arg(long, env = "DOMINIUM_METRICS_LISTEN", default_value = "0.0.0.0:9090")]
    pub metrics_listen: SocketAddr,

    /// Namespace holding the inventory ConfigMaps. Defaults to the pod's own namespace.
    #[arg(long, env = "DOMINIUM_NAMESPACE")]
    pub namespace: Option<String>,

    /// Settings file (registrars, prices, RDAP servers). Built-in defaults if unset.
    #[arg(long, env = "DOMINIUM_SETTINGS_FILE")]
    pub settings_file: Option<PathBuf>,

    /// Default warning threshold before expiry (skipped while auto-renew is on).
    #[arg(long, env = "DOMINIUM_WARN_BEFORE", default_value = "30d", value_parser = parse_duration)]
    pub warn_before: SignedDuration,

    /// Default critical threshold before expiry.
    #[arg(long, env = "DOMINIUM_CRITICAL_BEFORE", default_value = "7d", value_parser = parse_duration)]
    pub critical_before: SignedDuration,

    /// How often each domain is looked up in RDAP.
    #[arg(long, env = "DOMINIUM_RDAP_INTERVAL", default_value = "12h", value_parser = parse_duration)]
    pub rdap_interval: SignedDuration,

    /// IANA RDAP bootstrap file for domains.
    #[arg(long, env = "DOMINIUM_RDAP_BOOTSTRAP_URL", default_value = crate::rdap::IANA_BOOTSTRAP)]
    pub rdap_bootstrap_url: Url,

    /// Cloudflare account ID. With --cloudflare-token-file, enables the
    /// Registrar integration (auto-renew, lock, drift).
    #[arg(long, env = "DOMINIUM_CLOUDFLARE_ACCOUNT_ID")]
    pub cloudflare_account_id: Option<String>,

    /// File containing a read-only Cloudflare API token.
    #[arg(long, env = "DOMINIUM_CLOUDFLARE_TOKEN_FILE")]
    pub cloudflare_token_file: Option<PathBuf>,

    /// How often the Cloudflare account is listed.
    #[arg(long, env = "DOMINIUM_CLOUDFLARE_INTERVAL", default_value = "1h", value_parser = parse_duration)]
    pub cloudflare_interval: SignedDuration,

    #[arg(long, env = "DOMINIUM_CLOUDFLARE_API_URL", default_value = crate::cloudflare::API_BASE, hide = true)]
    pub cloudflare_api_url: Url,

    /// Serve sample data from memory, without Kubernetes or network access.
    #[arg(long, env = "DOMINIUM_DEMO")]
    pub demo: bool,

    /// Log as JSON.
    #[arg(long, env = "DOMINIUM_LOG_JSON")]
    pub log_json: bool,
}

fn parse_duration(s: &str) -> Result<SignedDuration, String> {
    duration::parse(s).map_err(|e| e.to_string())
}

impl Config {
    pub fn thresholds(&self) -> Thresholds {
        Thresholds {
            warn_before: self.warn_before,
            critical_before: self.critical_before,
        }
    }

    pub fn intervals(&self) -> Intervals {
        Intervals {
            rdap: self.rdap_interval,
            cloudflare: self.cloudflare_interval,
            ..Intervals::default()
        }
    }

    /// The namespace to watch: the flag, else the pod's own namespace.
    pub fn resolve_namespace(&self) -> anyhow::Result<String> {
        if let Some(ns) = &self.namespace {
            return Ok(ns.clone());
        }
        const SA_NAMESPACE: &str = "/var/run/secrets/kubernetes.io/serviceaccount/namespace";
        std::fs::read_to_string(SA_NAMESPACE)
            .map(|s| s.trim().to_string())
            .map_err(|e| anyhow::anyhow!("--namespace not set and {SA_NAMESPACE} unreadable: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let cfg = Config::try_parse_from(["dominium"]).unwrap();
        assert_eq!(cfg.thresholds(), Thresholds::default());
        assert_eq!(cfg.intervals().rdap, SignedDuration::from_hours(12));
        assert!(cfg.cloudflare_account_id.is_none());
    }
}
