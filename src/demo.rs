//! Sample data for `--demo`: an inventory, RDAP/Cloudflare answers relative
//! to now and a price table (illustrative prices only).

use std::collections::BTreeMap;
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use crate::cloudflare::{CfDomain, CfDomains};
use crate::facts::FactStore;
use crate::inventory::{Snapshot, from_configmaps};
use crate::rdap::RdapFacts;
use crate::settings::Settings;

const INVENTORY: &str = r#"{
  "version": 1,
  "domains": [
    {"name": "prdv.cloud", "registrar": "cloudflare", "purpose": "Homelab",
     "tags": ["infra"], "notes": "Zone, records and tunnels are managed by Pulumi."},
    {"name": "example.dev", "registrar": "cloudflare", "purpose": "Personal site", "tags": ["web"]},
    {"name": "old-blog.com", "registrar": "namecheap", "purpose": "Old blog, redirects to example.dev",
     "tags": ["web"], "autoRenew": false, "notes": "Transfer to Cloudflare before it renews."},
    {"name": "side-project.io", "registrar": "namecheap", "purpose": "Side project", "autoRenew": true},
    {"name": "shop-demo.net", "registrar": "porkbun", "tags": ["experiments"]},
    {"name": "exemplo.pt", "registrar": "ptisp", "purpose": "Portuguese landing page",
     "autoRenew": true, "expiresAt": "EXPIRES_PT", "renewal": {"price": 16.9, "currency": "EUR"}},
    {"name": "lapsed.org", "registrar": "porkbun", "tags": ["experiments"], "autoRenew": false},
    {"name": "typo-domain.com", "registrar": "cloudflare"}
  ]
}"#;

fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(24 * n)
}

fn rdap(expires: Timestamp, registered: Timestamp, registrar: &str, ns: &[&str]) -> RdapFacts {
    RdapFacts {
        expires_at: Some(expires),
        registered_at: Some(registered),
        updated_at: Some(registered + days(200)),
        registrar: Some(registrar.into()),
        statuses: vec!["client transfer prohibited".into()],
        nameservers: ns.iter().map(|s| s.to_string()).collect(),
        url: "https://rdap.example/domain".into(),
    }
}

pub fn inventory(now: Timestamp) -> Snapshot {
    let pt = (now + days(140)).strftime("%Y-%m-%d").to_string();
    let data = BTreeMap::from([(
        "domains.json".to_string(),
        INVENTORY.replace("EXPIRES_PT", &pt),
    )]);
    from_configmaps([("domains", &data)])
}

pub fn facts(now: Timestamp) -> Arc<FactStore> {
    let facts = FactStore::new(true);
    let cf_ns = ["ada.ns.cloudflare.com", "bob.ns.cloudflare.com"];
    let checked = now - SignedDuration::from_mins(42);
    let answers = [
        (
            "prdv.cloud",
            rdap(now + days(212), now - days(900), "Cloudflare, Inc.", &cf_ns),
        ),
        (
            "example.dev",
            rdap(now + days(19), now - days(1400), "Cloudflare, Inc.", &cf_ns),
        ),
        (
            "old-blog.com",
            rdap(
                now + days(23),
                now - days(3000),
                "NameCheap, Inc.",
                &["dns1.registrar-servers.com"],
            ),
        ),
        (
            "side-project.io",
            rdap(now + days(300), now - days(400), "Cloudflare, Inc.", &cf_ns),
        ),
        (
            "shop-demo.net",
            rdap(
                now + days(5),
                now - days(360),
                "Porkbun LLC",
                &["curitiba.ns.porkbun.com"],
            ),
        ),
        (
            "lapsed.org",
            rdap(now - days(3), now - days(730), "Porkbun LLC", &[]),
        ),
    ];
    for (name, f) in answers {
        facts.record_rdap(name, checked, Ok(f));
    }
    facts.record_rdap(
        "exemplo.pt",
        checked,
        Err("no RDAP server known for .pt; set expiresAt in the inventory or an RDAP server in the settings".into()),
    );
    facts.record_rdap(
        "typo-domain.com",
        checked,
        Err("the registry has no record of this domain".into()),
    );

    let cf = |expires: Timestamp, auto_renew: bool| CfDomain {
        expires_at: Some(expires),
        created_at: None,
        auto_renew: Some(auto_renew),
        locked: Some(true),
        privacy: Some(true),
        current_registrar: Some("Cloudflare".into()),
    };
    facts.record_cloudflare(
        checked,
        Ok(CfDomains::from([
            ("prdv.cloud".to_string(), cf(now + days(212), true)),
            ("example.dev".to_string(), cf(now + days(19), true)),
            ("side-project.io".to_string(), cf(now + days(300), true)),
            ("forgotten.xyz".to_string(), cf(now + days(90), false)),
        ])),
    );
    facts
}

pub fn settings() -> Settings {
    let mut s = Settings::default();
    let mut set = |key: &str, currency: &str, prices: &[(&str, f64)]| {
        let r = s.registrars.get_mut(key).expect("built-in registrar");
        r.currency = Some(currency.into());
        r.prices = prices.iter().map(|(t, p)| (t.to_string(), *p)).collect();
    };
    set(
        "cloudflare",
        "USD",
        &[("com", 10.44), ("dev", 12.2), ("cloud", 21.0), ("io", 50.0)],
    );
    set("namecheap", "USD", &[("com", 15.98), ("io", 61.98)]);
    set("porkbun", "USD", &[("net", 12.52), ("org", 10.74)]);
    s
}
