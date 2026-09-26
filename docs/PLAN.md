# dominium — implementation plan

A self-hosted, Kubernetes-native inventory of my domains: where each one is
registered, when it expires, whether it renews on its own and what the renewal
costs. Expiry is exported as Prometheus metrics, so alerts arrive through
Alertmanager, like [you-spin-me](https://github.com/PedDavid/you-spin-me)'s.

## 1. Decisions

| Topic | Decision |
|---|---|
| Scope | Domains only: expiry, auto-renew, registrar, renewal price, notes |
| Platform | Kubernetes only, same stack and UI as you-spin-me |
| Inventory (which domains exist) | ConfigMaps written by the Pulumi (Kotlin) program that already manages the domains. The app never declares domains itself, so nothing is written twice |
| Facts (expiry, registrar, statuses) | Looked up by the app: RDAP for every domain (public, no credentials), plus the Cloudflare Registrar API when a read-only token is configured |
| State | In memory only. Everything is either in a ConfigMap or can be fetched again, so a restart or a cluster rebuild loses nothing |
| Prices | A per-registrar, per-TLD table in the Helm values, overridable per domain in the inventory |
| Auth | None. The data is public. Put it behind the ingress's forward-auth if wanted |
| Secrets | None required. The Cloudflare token is optional and read-only |
| Alerting | Prometheus metrics plus a shipped `PrometheusRule`. No notifier in the app |

Registrars in use today: Cloudflare, Namecheap, Porkbun and PTisp. The plan is
to move everything to Cloudflare over time, so Cloudflare gets the extra API
integration and the others rely on RDAP and on what the inventory says.

## 2. Architecture

```
Pulumi (Kotlin) ──▶ ConfigMap(s) labelled dominium.prdv.cloud/inventory=true
                          │ watch (Role: get/list/watch configmaps in one namespace)
                          ▼
                 ┌──────────────────────┐        ┌──────────────────────────────┐
                 │ dominium             │──────▶ │ RDAP (IANA bootstrap)        │ expiry, created,
                 │  • ConfigMap cache   │        └──────────────────────────────┘ registrar, statuses, NS
                 │  • fact refresher    │        ┌──────────────────────────────┐
                 │  • axum: UI, metrics │──────▶ │ Cloudflare Registrar API     │ auto-renew, lock,
                 └──────────┬───────────┘        │ (optional read-only token)   │ expiry, drift
                            │ /metrics           └──────────────────────────────┘
                            ▼
                 Prometheus ──▶ Alertmanager
```

- **One replica**, no database, no leader election. Nothing is written anywhere.
- **Refresh:** RDAP every 12h per domain (spread out by a per-domain offset,
  one request at a time), retried after 1h on failure. Cloudflare every hour.
  The IANA bootstrap file is fetched at start and daily.
- **Metrics** are computed from the caches on every scrape.

## 3. The inventory ConfigMap

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: domains
  namespace: dominium
  labels:
    dominium.prdv.cloud/inventory: "true"
data:
  domains.json: |
    {
      "version": 1,
      "domains": [
        {
          "name": "prdv.cloud",
          "registrar": "cloudflare",
          "purpose": "Homelab",
          "tags": ["infra"],
          "notes": "Zone and records managed by Pulumi.",
          "autoRenew": true,
          "renewal": { "price": 10.44, "currency": "USD" },
          "expiresAt": "2027-03-01",
          "alerts": { "warnBefore": "30d", "criticalBefore": "7d" }
        }
      ]
    }
```

- Only `name` and `registrar` are required. `registrar` is a key into the
  registrar table (`cloudflare`, `namecheap`, `porkbun`, `ptisp`, …). Unknown
  keys are allowed and shown as they are.
- `autoRenew` is what I believe is configured at the registrar. For Cloudflare
  domains, the API's value wins when a token is configured.
- `expiresAt` is a fallback for TLDs without RDAP. RDAP and Cloudflare win when
  they answer.
- The app reads every ConfigMap with the label in its namespace and merges them
  (for example one per Pulumi stack). Any key ending in `.json` is read. A
  domain declared twice, an invalid name or unparseable JSON is shown in the UI
  and exported as `dominium_inventory_problems`.
- `version` lets the format change later without guessing.

On the Pulumi side this is one `ConfigMap` resource whose data is the domain
list serialised with `kotlinx.serialization` (an example is in the README).

## 4. Facts and precedence

| Fact | Precedence |
|---|---|
| Expiry | Cloudflare API (Cloudflare domains) → RDAP → `expiresAt` from the inventory |
| Auto-renew | Cloudflare API → `autoRenew` from the inventory → unknown |
| Price | `renewal` from the inventory → registrar/TLD table → unknown |
| Registrar | Declared in the inventory; RDAP's registrar name is compared with it |

Drift checks, all shown in the UI and exported as metrics:

- **Registrar mismatch:** RDAP names a different registrar than the inventory,
  e.g. a transfer to Cloudflare finished but the inventory was not updated.
- **Not in Cloudflare:** declared as `cloudflare` but not in the account.
- **Undeclared:** in the Cloudflare account but in no inventory ConfigMap.

## 5. States and alerts

The state is computed from the effective expiry and per-domain thresholds
(defaults: warning 30d, critical 7d):

| State | Condition |
|---|---|
| Expired | expiry has passed |
| Critical | less than `criticalBefore` left, auto-renew or not: if it's still this close, a renewal failed |
| Warning | less than `warnBefore` left and auto-renew is not known to be on |
| Unknown | no expiry from any source |
| OK | otherwise |

The shipped rules follow the same logic (`DomainExpiringSoon`,
`DomainExpiringVerySoon`, `DomainExpired`, `DomainExpiryUnknown`,
`DomainRegistrarMismatch`, `DomainUndeclared`, `DomainInventoryInvalid`,
`DominiumDown`).

## 6. Metrics

Per-domain metrics carry `domain` and `registrar`.

| Metric | |
|---|---|
| `dominium_domain_expiry_timestamp_seconds` | Effective expiry |
| `dominium_domain_auto_renew` | 1 or 0; absent when unknown |
| `dominium_domain_renewal_price{currency}` | Yearly renewal price |
| `dominium_domain_warn_before_seconds`, `…_critical_before_seconds` | Thresholds |
| `dominium_domain_state_known` | 0 when no source knows the expiry |
| `dominium_domain_registrar_mismatch{rdap_registrar}` | 1 when RDAP disagrees |
| `dominium_domain_missing_from_cloudflare` | 1 when declared as Cloudflare but absent |
| `dominium_domain_last_checked_timestamp_seconds{source}` | Last successful RDAP/Cloudflare answer |
| `dominium_domain_info{purpose, source}` | Metadata for joins |
| `dominium_undeclared_domain{domain}` | In Cloudflare, not in the inventory |
| `dominium_inventory_problems` | Invalid or duplicate entries |
| `dominium_rdap_requests_total{result}`, `dominium_cloudflare_requests_total{result}` | |

## 7. UI

Same look as you-spin-me: askama templates, Tailwind v4, vendored Basecoat and
htmx, the ⌘K palette, dark mode and accent palettes. No login, no forms, no
POST routes.

- **Domains:** summary (count, next expiry, yearly cost per currency), state
  chips, filters (text, registrar, tag), sort; table with domain, state, expiry,
  auto-renew, registrar, price. Problems and drift are listed above the table.
- **Domain detail:** renewal (expiry and its source, auto-renew, price,
  thresholds), registration (RDAP registrar, created, updated, statuses,
  nameservers, last check or error), inventory (ConfigMap, purpose, tags,
  notes), and a link to the registrar's dashboard.

## 8. Not doing (for now)

- Registrar APIs other than Cloudflare. Namecheap's API needs an IP allow-list
  and Porkbun's needs keys per account; RDAP already gives the expiry.
- Renewing or changing anything at a registrar. The app is read-only.
- Fetching prices from the Cloudflare API. Whether it exposes renewal prices is
  unverified; the table is a few lines to maintain.
- Certificates or DNS record checks: Pulumi owns DNS.
