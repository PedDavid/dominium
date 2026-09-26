//! The UI and ops routers against the demo data.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use dominium::demo;
use dominium::inventory::StaticInventory;
use dominium::metrics::{Counters, Metrics};
use dominium::status::{Domains, Thresholds};
use dominium::web::{AppState, ops_router, router};

fn domains() -> Arc<Domains> {
    let now = jiff::Timestamp::now();
    Arc::new(Domains {
        inventory: Arc::new(StaticInventory(demo::inventory(now))),
        facts: demo::facts(now),
        settings: demo::settings(),
        thresholds: Thresholds::default(),
    })
}

async fn get(
    app: axum::Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut req = Request::get(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn index_lists_domains_and_drift() {
    let app = router(AppState::new(domains(), true));
    let (status, headers, body) = get(app, "/", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    for expected in [
        "prdv.cloud",
        "exemplo.pt",
        "forgotten.xyz",
        "Namecheap",
        "€16.90",
        "data-state=\"expired\"",
    ] {
        assert!(body.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn filters_return_only_the_table_for_htmx() {
    let app = router(AppState::new(domains(), true));
    let (status, _, body) = get(
        app,
        "/?registrar=porkbun&state=expired",
        &[("hx-request", "true"), ("hx-target", "domains-table")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("<html"));
    assert!(body.contains("lapsed.org"));
    assert!(!body.contains("shop-demo.net\n") && !body.contains(">shop-demo.net<"));
    assert!(body.contains("hx-swap-oob"));
}

#[tokio::test]
async fn detail_and_not_found() {
    let app = router(AppState::new(domains(), true));
    let (status, _, body) = get(app.clone(), "/domains/side-project.io", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Registrar mismatch"));
    assert!(body.contains("Cloudflare, Inc."));
    let (status, _, body) = get(app, "/domains/nope.com", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("nope.com"));
}

#[tokio::test]
async fn search_orders_by_expiry() {
    let app = router(AppState::new(domains(), true));
    let (_, _, body) = get(app.clone(), "/search", &[]).await;
    let lapsed = body.find("lapsed.org").unwrap();
    let prdv = body.find("prdv.cloud").unwrap();
    assert!(lapsed < prdv);
    let (_, _, body) = get(app, "/search?q=homelab", &[]).await;
    assert!(body.contains("prdv.cloud") && !body.contains("lapsed.org"));
}

#[tokio::test]
async fn assets_are_served_and_cached() {
    let app = router(AppState::new(domains(), true));
    let (status, headers, _) = get(app.clone(), "/assets/dist/app.css", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    let (status, _, _) = get(app, "/assets/../Cargo.toml", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ops_endpoints() {
    let d = domains();
    let counters = Counters::default();
    let ops = ops_router(d.clone(), Metrics::new(d, &counters));
    let (status, _, body) = get(ops.clone(), "/metrics", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"dominium_undeclared_domain{domain="forgotten.xyz"} 1"#));
    let (status, _, _) = get(ops, "/readyz", &[]).await;
    assert_eq!(status, StatusCode::OK);
}
