use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use dominium::cloudflare::{CloudflareClient, Registrar};
use dominium::config::Config;
use dominium::demo;
use dominium::facts::{FactStore, Refresher};
use dominium::inventory::{Inventory, StaticInventory};
use dominium::k8s::KubeInventory;
use dominium::metrics::{Counters, Metrics};
use dominium::rdap::RdapClient;
use dominium::settings::Settings;
use dominium::status::Domains;
use dominium::web::{self, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::parse();
    init_tracing(cfg.log_json);

    let counters = Arc::new(Counters::default());
    let domains = if cfg.demo {
        warn!("demo mode: serving sample data from memory");
        let now = jiff::Timestamp::now();
        Arc::new(Domains {
            inventory: Arc::new(StaticInventory(demo::inventory(now))),
            facts: demo::facts(now),
            settings: demo::settings(),
            thresholds: cfg.thresholds(),
        })
    } else {
        let settings = Settings::load(cfg.settings_file.as_deref())?;
        let namespace = cfg.resolve_namespace()?;
        let client = kube::Client::try_default()
            .await
            .context("connecting to Kubernetes")?;
        info!(%namespace, "watching inventory ConfigMaps");
        let inventory: Arc<dyn Inventory> = KubeInventory::start(client, &namespace);
        let cloudflare = cloudflare(&cfg)?;
        let facts = FactStore::new(cloudflare.is_some());
        Refresher {
            inventory: inventory.clone(),
            facts: facts.clone(),
            rdap: Arc::new(RdapClient::new(
                cfg.rdap_bootstrap_url.clone(),
                settings.rdap.servers.clone(),
            )?),
            cloudflare,
            counters: counters.clone(),
            intervals: cfg.intervals(),
        }
        .spawn();
        Arc::new(Domains {
            inventory,
            facts,
            settings,
            thresholds: cfg.thresholds(),
        })
    };

    let metrics = Metrics::new(domains.clone(), &counters);
    let ops = web::ops_router(domains.clone(), metrics);
    let app = web::router(AppState::new(domains, cfg.demo));

    let ui_listener = tokio::net::TcpListener::bind(cfg.listen).await?;
    let ops_listener = tokio::net::TcpListener::bind(cfg.metrics_listen).await?;
    info!(ui = %cfg.listen, ops = %cfg.metrics_listen, "listening");
    tokio::select! {
        r = axum::serve(ui_listener, app).with_graceful_shutdown(shutdown()) => r?,
        r = axum::serve(ops_listener, ops).with_graceful_shutdown(shutdown()) => r?,
    }
    Ok(())
}

fn cloudflare(cfg: &Config) -> anyhow::Result<Option<Arc<dyn Registrar>>> {
    match (&cfg.cloudflare_account_id, &cfg.cloudflare_token_file) {
        (Some(account), Some(path)) => {
            let token = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            info!("Cloudflare Registrar integration enabled");
            Ok(Some(Arc::new(CloudflareClient::new(
                cfg.cloudflare_api_url.clone(),
                account.clone(),
                token,
            )?)))
        }
        (None, None) => {
            info!("no Cloudflare token: using RDAP and the inventory only");
            Ok(None)
        }
        _ => anyhow::bail!("--cloudflare-account-id and --cloudflare-token-file go together"),
    }
}

fn init_tracing(json: bool) {
    let filter = EnvFilter::try_from_env("DOMINIUM_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,kube=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
