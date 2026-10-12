//! Exchange rates for estimating totals in one currency: the ECB's daily
//! reference rates (public, no key), plus fixed rates from the settings.
//! All rates are expressed per 1 EUR, as the ECB publishes them.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use jiff::civil::Date;
use url::Url;

use crate::inventory::Price;

pub const ECB_DAILY: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";

/// Whether `code` looks like an ISO 4217 code (`EUR`, `USD`, …).
pub fn valid_code(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())
}

#[derive(Clone, Debug, PartialEq)]
pub struct FetchedRates {
    /// The ECB's reference date.
    pub date: Option<Date>,
    /// Units of each currency per 1 EUR.
    pub per_eur: BTreeMap<String, f64>,
}

#[async_trait]
pub trait RatesSource: Send + Sync {
    async fn fetch(&self) -> Result<FetchedRates, String>;
}

pub struct EcbClient {
    http: reqwest::Client,
    url: Url,
}

impl EcbClient {
    pub fn new(url: Url) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("dominium/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(EcbClient { http, url })
    }
}

#[async_trait]
impl RatesSource for EcbClient {
    async fn fetch(&self) -> Result<FetchedRates, String> {
        let body = self
            .http
            .get(self.url.clone())
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("fetching exchange rates: {e}"))?
            .text()
            .await
            .map_err(|e| format!("reading exchange rates: {e}"))?;
        let rates = parse_ecb(&body);
        if rates.per_eur.is_empty() {
            return Err("the exchange rate file has no rates".into());
        }
        Ok(rates)
    }
}

/// The value of `name='…'` (or `"…"`) inside one XML tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let preceded_by_space = rest[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let after = &rest[at + name.len()..];
        if preceded_by_space && let Some(value) = after.strip_prefix('=') {
            let quote = value.chars().next()?;
            if quote != '\'' && quote != '"' {
                return None;
            }
            let value = &value[1..];
            return value.find(quote).map(|end| &value[..end]);
        }
        rest = after;
    }
}

/// Parses the ECB's `eurofxref-daily.xml`:
/// `<Cube time='2026-10-09'><Cube currency='USD' rate='1.0812'/>…`.
/// No XML library: the format is three levels of `Cube` tags.
pub fn parse_ecb(xml: &str) -> FetchedRates {
    let mut date = None;
    let mut per_eur = BTreeMap::new();
    for tag in xml.split('<').map(|t| t.split('>').next().unwrap_or("")) {
        if !tag.starts_with("Cube") {
            continue;
        }
        if let Some(t) = attr(tag, "time") {
            date = t.parse().ok().or(date);
        }
        if let (Some(c), Some(r)) = (attr(tag, "currency"), attr(tag, "rate"))
            && valid_code(c)
            && let Ok(r) = r.parse::<f64>()
            && r.is_finite()
            && r > 0.0
        {
            per_eur.insert(c.to_string(), r);
        }
    }
    FetchedRates { date, per_eur }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// EUR itself.
    Base,
    Ecb(Option<Date>),
    Fixed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rate {
    per_eur: f64,
    origin: Origin,
}

/// The rates in effect: the fetched ones, overridden by fixed ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateTable {
    rates: BTreeMap<String, Rate>,
}

impl RateTable {
    pub fn new(fetched: Option<&FetchedRates>, fixed: &BTreeMap<String, f64>) -> RateTable {
        let mut rates = BTreeMap::from([(
            "EUR".to_string(),
            Rate {
                per_eur: 1.0,
                origin: Origin::Base,
            },
        )]);
        if let Some(f) = fetched {
            for (c, r) in &f.per_eur {
                rates.insert(
                    c.clone(),
                    Rate {
                        per_eur: *r,
                        origin: Origin::Ecb(f.date),
                    },
                );
            }
        }
        for (c, r) in fixed {
            if c != "EUR" {
                rates.insert(
                    c.clone(),
                    Rate {
                        per_eur: *r,
                        origin: Origin::Fixed,
                    },
                );
            }
        }
        RateTable { rates }
    }

    pub fn currencies(&self) -> impl Iterator<Item = &str> {
        self.rates.keys().map(String::as_str)
    }

    /// Units of `to` per 1 `from`.
    pub fn rate(&self, from: &str, to: &str) -> Option<f64> {
        Some(self.rates.get(to)?.per_eur / self.rates.get(from)?.per_eur)
    }

    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<f64> {
        if from == to {
            return Some(amount);
        }
        self.rate(from, to).map(|r| amount * r)
    }

    /// Where the rate between two currencies comes from, for humans.
    pub fn origin(&self, from: &str, to: &str) -> String {
        let origins = [from, to].map(|c| self.rates.get(c).map(|r| r.origin));
        if origins.contains(&Some(Origin::Fixed)) {
            return "fixed rate from the settings".into();
        }
        let date = origins.iter().find_map(|o| match o {
            Some(Origin::Ecb(d)) => Some(*d),
            _ => None,
        });
        match date {
            Some(Some(d)) => format!("ECB reference rate of {d}"),
            _ => "ECB reference rate".into(),
        }
    }
}

/// One currency's share of an estimate.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub currency: String,
    pub amount: f64,
    /// `None` when there is no rate for this currency.
    pub converted: Option<f64>,
    /// Units of the target per 1 of this currency.
    pub rate: Option<f64>,
    pub origin: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    pub currency: String,
    /// Sum of the converted parts.
    pub total: f64,
    /// Per source currency, largest first.
    pub parts: Vec<Part>,
}

impl Estimate {
    pub fn compute<'a>(
        prices: impl IntoIterator<Item = &'a Price>,
        table: &RateTable,
        currency: &str,
    ) -> Estimate {
        let mut sums: BTreeMap<&str, f64> = BTreeMap::new();
        for p in prices {
            *sums.entry(p.currency.as_str()).or_default() += p.amount;
        }
        let mut parts: Vec<Part> = sums
            .into_iter()
            .map(|(c, amount)| {
                let same = c == currency;
                Part {
                    currency: c.to_string(),
                    amount,
                    converted: table.convert(amount, c, currency),
                    rate: (!same).then(|| table.rate(c, currency)).flatten(),
                    origin: if same {
                        String::new()
                    } else {
                        table.origin(c, currency)
                    },
                }
            })
            .collect();
        parts.sort_by(|a, b| {
            let key = |p: &Part| p.converted.unwrap_or(f64::NEG_INFINITY);
            key(b)
                .total_cmp(&key(a))
                .then_with(|| a.currency.cmp(&b.currency))
        });
        Estimate {
            currency: currency.to_string(),
            total: parts.iter().filter_map(|p| p.converted).sum(),
            parts,
        }
    }

    /// Whether any amount had to be converted.
    pub fn is_estimate(&self) -> bool {
        self.parts.iter().any(|p| p.currency != self.currency)
    }

    pub fn missing_rates(&self) -> bool {
        self.parts.iter().any(|p| p.converted.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ECB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
	<gesmes:subject>Reference rates</gesmes:subject>
	<Cube>
		<Cube time='2026-10-09'>
			<Cube currency='USD' rate='1.0812'/>
			<Cube currency="GBP" rate="0.8371"/>
			<Cube currency='JPY' rate='162.10'/>
			<Cube currency='bad' rate='1'/>
			<Cube currency='XXX' rate='-1'/>
		</Cube>
	</Cube>
</gesmes:Envelope>"#;

    fn price(amount: f64, currency: &str) -> Price {
        Price {
            amount,
            currency: currency.into(),
        }
    }

    #[test]
    fn parses_ecb_daily() {
        let r = parse_ecb(ECB);
        assert_eq!(r.date, Some("2026-10-09".parse().unwrap()));
        assert_eq!(r.per_eur.len(), 3);
        assert_eq!(r.per_eur["USD"], 1.0812);
        assert_eq!(r.per_eur["GBP"], 0.8371);
        assert!(parse_ecb("not xml").per_eur.is_empty());
    }

    #[test]
    fn attributes_need_a_word_boundary() {
        assert_eq!(
            attr("Cube xcurrency='A' currency='USD'", "currency"),
            Some("USD")
        );
        assert_eq!(attr("Cube currency=USD", "currency"), None);
    }

    #[test]
    fn converts_through_eur_and_fixed_rates_win() {
        let fetched = parse_ecb(ECB);
        let fixed = BTreeMap::from([("CHF".to_string(), 0.94), ("GBP".to_string(), 0.85)]);
        let t = RateTable::new(Some(&fetched), &fixed);
        assert!((t.convert(10.812, "USD", "EUR").unwrap() - 10.0).abs() < 1e-9);
        assert!((t.convert(10.0, "EUR", "USD").unwrap() - 10.812).abs() < 1e-9);
        assert!((t.rate("USD", "GBP").unwrap() - 0.85 / 1.0812).abs() < 1e-9);
        assert_eq!(t.convert(5.0, "BRL", "EUR"), None);
        assert_eq!(t.convert(5.0, "BRL", "BRL"), Some(5.0));
        assert_eq!(t.origin("USD", "EUR"), "ECB reference rate of 2026-10-09");
        assert_eq!(t.origin("GBP", "EUR"), "fixed rate from the settings");
        assert_eq!(
            t.currencies().collect::<Vec<_>>(),
            vec!["CHF", "EUR", "GBP", "JPY", "USD"]
        );
        // Without fetched rates only EUR and the fixed ones are known.
        let t = RateTable::new(None, &BTreeMap::new());
        assert_eq!(t.currencies().collect::<Vec<_>>(), vec!["EUR"]);
    }

    #[test]
    fn estimates_in_one_currency() {
        let t = RateTable::new(Some(&parse_ecb(ECB)), &BTreeMap::new());
        let prices = [
            price(10.812, "USD"),
            price(10.812, "USD"),
            price(16.9, "EUR"),
            price(3.0, "BRL"),
        ];
        let e = Estimate::compute(&prices, &t, "EUR");
        assert!(e.is_estimate() && e.missing_rates());
        assert!((e.total - 36.9).abs() < 1e-9);
        assert_eq!(e.parts[0].currency, "USD");
        assert!((e.parts[0].converted.unwrap() - 20.0).abs() < 1e-9);
        assert!((e.parts[0].rate.unwrap() - 1.0 / 1.0812).abs() < 1e-9);
        assert_eq!(e.parts[1].currency, "EUR");
        assert_eq!(e.parts[1].rate, None);
        assert_eq!(e.parts[2].currency, "BRL");
        assert_eq!(e.parts[2].converted, None);

        let e = Estimate::compute(&prices[2..3], &t, "EUR");
        assert!(!e.is_estimate());
    }

    #[tokio::test]
    async fn fetches_from_the_ecb_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB))
            .mount(&server)
            .await;
        let client = EcbClient::new(server.uri().parse().unwrap()).unwrap();
        assert_eq!(client.fetch().await.unwrap().per_eur["USD"], 1.0812);

        let empty = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html/>"))
            .mount(&empty)
            .await;
        let client = EcbClient::new(empty.uri().parse().unwrap()).unwrap();
        assert!(client.fetch().await.is_err());
    }
}
