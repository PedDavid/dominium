//! Rendered HTML of each page, pinned with insta. Review changes with
//! `cargo insta review`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn render(uri: &str, headers: &[(&str, &str)]) -> String {
    let mut req = Request::get(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = common::app()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        res.status() == StatusCode::OK || res.status() == StatusCode::NOT_FOUND,
        "{uri}: {}",
        res.status()
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

/// The asset hash changes with every CSS or JS edit; keep it out of the
/// snapshots so they only move when the markup does.
fn assert_html(name: &str, html: String) {
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(r"\?v=[0-9a-f]+", "?v=[hash]");
    settings.bind(|| insta::assert_snapshot!(name, html));
}

#[tokio::test]
async fn index() {
    assert_html("index", render("/", &[]).await);
}

#[tokio::test]
async fn index_dark_with_palette() {
    let html = render(
        "/",
        &[("cookie", "dominium_theme=dark; dominium_palette=violet")],
    )
    .await;
    assert_html("index_dark_violet", html);
}

#[tokio::test]
async fn filtered_table_for_htmx() {
    let html = render(
        "/?registrar=porkbun&state=expired&sort=name",
        &[("hx-request", "true"), ("hx-target", "domains-table")],
    )
    .await;
    assert_html("table_porkbun_expired", html);
}

#[tokio::test]
async fn detail_with_drift() {
    assert_html(
        "detail_side_project",
        render("/domains/side-project.io", &[]).await,
    );
}

#[tokio::test]
async fn search_results() {
    assert_html("search_empty", render("/search", &[]).await);
    assert_html("search_homelab", render("/search?q=homelab", &[]).await);
}

#[tokio::test]
async fn not_found() {
    assert_html("not_found", render("/domains/nope.com", &[]).await);
}
