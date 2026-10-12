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

#[tokio::test]
async fn totals_are_estimated_in_the_chosen_currency() {
    let app = router(AppState::new(domains(), true));
    // Demo default: EUR. $144.86 at 1.0812 USD/EUR + €16.90.
    let (_, _, body) = get(app.clone(), "/", &[]).await;
    assert!(body.contains("≈ €150.88"), "EUR total");
    assert!(body.contains("Estimate in EUR at current rates"));
    assert!(body.contains("1 USD = 0.9249 EUR · ECB reference rate of"));
    assert!(body.contains(r#"<option value="EUR" selected>"#));
    assert!(body.contains(r#"<option value="BRL" >"#) || body.contains(r#"<option value="BRL">"#));

    let (_, _, body) = get(app.clone(), "/", &[("cookie", "dominium_currency=USD")]).await;
    assert!(body.contains("≈ $163.13"), "USD total");
    assert!(body.contains("1 EUR = 1.0812 USD"));

    // Unknown or malformed currencies fall back to the default.
    for cookie in [
        "dominium_currency=XYZ",
        "dominium_currency=usd",
        "dominium_currency=%3Cb%3E",
    ] {
        let (_, _, body) = get(app.clone(), "/", &[("cookie", cookie)]).await;
        assert!(body.contains("≈ €150.88"), "{cookie}");
    }
}

#[tokio::test]
async fn price_sort_compares_converted_amounts() {
    let app = router(AppState::new(domains(), true));
    let (_, _, body) = get(
        app,
        "/?sort=price",
        &[("hx-request", "true"), ("hx-target", "domains-table")],
    )
    .await;
    // In EUR: side-project.io ($61.98 ≈ €57.33), prdv.cloud ($21.00 ≈ €19.42),
    // exemplo.pt (€16.90), old-blog.com ($15.98 ≈ €14.78).
    let pos = |n: &str| body.find(&format!(">{n}<")).unwrap();
    assert!(pos("side-project.io") < pos("prdv.cloud"));
    assert!(pos("prdv.cloud") < pos("exemplo.pt"));
    assert!(pos("exemplo.pt") < pos("old-blog.com"));
}
