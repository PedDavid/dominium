//! Page handlers.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use serde::Deserialize;

use super::views::{DomainDetail, DomainRow, yearly_totals};
use super::{AppError, AppState};
use crate::inventory::Problem;
use crate::status::State as DomainState;

/// Data shared by every full page.
pub struct Layout {
    pub title: String,
    pub asset_version: String,
    pub dark: bool,
    pub palette: String,
    pub demo: bool,
}

pub const PALETTES: [&str; 5] = ["neutral", "blue", "green", "orange", "violet"];

impl Layout {
    fn new(state: &AppState, jar: &CookieJar, title: impl Into<String>) -> Layout {
        let palette = jar
            .get("dominium_palette")
            .map(|c| c.value().to_string())
            .filter(|p| PALETTES.contains(&p.as_str()))
            .unwrap_or_else(|| "neutral".into());
        Layout {
            title: title.into(),
            asset_version: state.inner.asset_version.clone(),
            dark: jar
                .get("dominium_theme")
                .is_some_and(|c| c.value() == "dark"),
            palette,
            demo: state.inner.demo,
        }
    }
}

#[derive(Deserialize, Default, Clone)]
pub struct Filters {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub registrar: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub sort: String,
}

pub struct StateCount {
    pub state: &'static str,
    pub label: &'static str,
    pub count: usize,
    pub active: bool,
}

pub struct Summary {
    pub total: usize,
    pub attention: usize,
    pub next_name: String,
    pub next_rel: String,
    pub next_date: String,
    pub yearly: Vec<String>,
    pub unpriced: usize,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexPage {
    layout: Layout,
    summary: Summary,
    problems: Vec<Problem>,
    undeclared: Vec<String>,
    rows: Vec<DomainRow>,
    total: usize,
    matching: usize,
    counts: Vec<StateCount>,
    registrars: Vec<(String, String)>,
    tags: Vec<String>,
    filters: Filters,
    oob: bool,
}

#[derive(Template)]
#[template(path = "partials/domains_table.html")]
struct DomainsTable {
    rows: Vec<DomainRow>,
    total: usize,
    matching: usize,
    counts: Vec<StateCount>,
    filters: Filters,
    /// Also re-render the state chips as an htmx out-of-band swap.
    oob: bool,
}

fn is_htmx(headers: &HeaderMap, target: &str) -> bool {
    headers.contains_key("hx-request")
        && headers
            .get("hx-target")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t == target)
}

fn summary(all: &[DomainRow]) -> Summary {
    let next = all
        .iter()
        .filter(|r| r.state != DomainState::Expired)
        .filter_map(|r| r.expires_at.map(|t| (t, r)))
        .min_by_key(|(t, _)| *t)
        .map(|(_, r)| r);
    Summary {
        total: all.len(),
        attention: all
            .iter()
            .filter(|r| r.state != DomainState::Ok || r.drift)
            .count(),
        next_name: next.map(|r| r.name.clone()).unwrap_or_default(),
        next_rel: next.map(|r| r.expires_rel.clone()).unwrap_or_default(),
        next_date: next.map(|r| r.expires_date.clone()).unwrap_or_default(),
        yearly: yearly_totals(all),
        unpriced: all.iter().filter(|r| r.price.is_none()).count(),
    }
}

pub async fn index(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Query(filters): Query<Filters>,
) -> Result<Response, AppError> {
    let now = Timestamp::now();
    let overview = state.inner.domains.overview();
    let all: Vec<DomainRow> = overview
        .domains
        .iter()
        .map(|d| DomainRow::new(d, now))
        .collect();
    let total = all.len();
    let wanted_state = DomainState::parse(&filters.state);

    let mut registrars: Vec<(String, String)> = all
        .iter()
        .map(|r| (r.registrar.clone(), r.registrar_name.clone()))
        .collect();
    registrars.sort();
    registrars.dedup();
    let mut tags: Vec<String> = all.iter().flat_map(|r| r.tags.clone()).collect();
    tags.sort();
    tags.dedup();

    let searched: Vec<DomainRow> = all
        .iter()
        .filter(|r| filters.q.is_empty() || r.matches(&filters.q))
        .filter(|r| filters.registrar.is_empty() || r.registrar == filters.registrar)
        .filter(|r| filters.tag.is_empty() || r.tags.contains(&filters.tag))
        .cloned()
        .collect();
    let matching = searched.len();
    let counts = DomainState::ALL
        .iter()
        .map(|s| StateCount {
            state: s.as_str(),
            label: s.label(),
            count: searched.iter().filter(|r| r.state == *s).count(),
            active: wanted_state == Some(*s),
        })
        .collect();
    let mut rows: Vec<DomainRow> = searched
        .into_iter()
        .filter(|r| wanted_state.is_none_or(|s| r.state == s))
        .collect();
    sort_rows(&mut rows, &filters.sort);

    if is_htmx(&headers, "domains-table") {
        return Ok(Html(
            DomainsTable {
                rows,
                total,
                matching,
                counts,
                filters,
                oob: true,
            }
            .render()?,
        )
        .into_response());
    }
    Ok(Html(
        IndexPage {
            layout: Layout::new(&state, &jar, "Domains"),
            summary: summary(&all),
            problems: overview.snapshot.problems,
            undeclared: overview.undeclared,
            rows,
            total,
            matching,
            counts,
            registrars,
            tags,
            filters,
            oob: false,
        }
        .render()?,
    )
    .into_response())
}

fn sort_rows(rows: &mut [DomainRow], sort: &str) {
    match sort {
        "name" => rows.sort_by(|a, b| a.name.cmp(&b.name)),
        "registrar" => rows.sort_by(|a, b| {
            (a.registrar_name.to_lowercase(), &a.name)
                .cmp(&(b.registrar_name.to_lowercase(), &b.name))
        }),
        // Most expensive first; unpriced last.
        "price" => rows.sort_by(|a, b| {
            let amount = |r: &DomainRow| r.price.as_ref().map(|p| p.amount);
            amount(b)
                .partial_cmp(&amount(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        }),
        // Default: most urgent first, unknown expiries after known ones.
        _ => rows.sort_by(|a, b| match (a.expires_at, b.expires_at) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        }),
    }
}

#[derive(Template)]
#[template(path = "detail.html")]
struct DetailPage {
    layout: Layout,
    d: DomainDetail,
    cloudflare_enabled: bool,
}

pub async fn detail(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let overview = state.inner.domains.overview();
    let domain = overview
        .domains
        .iter()
        .find(|d| d.name() == name)
        .ok_or_else(|| AppError::NotFound(format!("no domain named {name:?} in the inventory")))?;
    let d = DomainDetail::new(domain, Timestamp::now());
    Ok(Html(
        DetailPage {
            layout: Layout::new(&state, &jar, d.row.name.clone()),
            cloudflare_enabled: state.inner.domains.facts.cloudflare_enabled(),
            d,
        }
        .render()?,
    )
    .into_response())
}

#[derive(Deserialize, Default)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

#[derive(Template)]
#[template(path = "partials/search_results.html")]
struct SearchResults {
    q: String,
    rows: Vec<DomainRow>,
}

/// Results for the command palette: matches for `q`, or the next expiries.
pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let now = Timestamp::now();
    let q = query.q.trim().to_string();
    let mut rows: Vec<DomainRow> = state
        .inner
        .domains
        .overview()
        .domains
        .iter()
        .map(|d| DomainRow::new(d, now))
        .filter(|r| q.is_empty() || r.matches(&q))
        .collect();
    sort_rows(&mut rows, "");
    rows.truncate(8);
    Ok(Html(SearchResults { q, rows }.render()?).into_response())
}
