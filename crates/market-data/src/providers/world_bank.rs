//! World Bank Indicators API v2 adapter (official, no API key required).

use std::sync::Arc;

use astock_core::DataError;
use serde::{Deserialize, Serialize};

use crate::HttpClient;

const BASE: &str = "https://api.worldbank.org/v2";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldBankObservation {
    pub indicator_code: String,
    pub indicator_name: String,
    pub country_id: String,
    pub country_iso3: String,
    pub country_name: String,
    pub period: String,
    pub value: Option<f64>,
    pub unit: String,
    pub decimal_places: u32,
    pub observation_status: String,
}

#[derive(Clone)]
pub struct WorldBankProvider {
    http: Arc<HttpClient>,
}

impl WorldBankProvider {
    pub fn new(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    pub async fn latest(
        &self,
        countries: &[&str],
        indicator: &str,
        recent_values: u32,
    ) -> Result<Vec<WorldBankObservation>, DataError> {
        let countries = countries.join(";");
        let url = format!("{BASE}/country/{countries}/indicator/{indicator}");
        let params = vec![
            ("format".into(), "json".into()),
            ("mrv".into(), recent_values.clamp(1, 50).to_string()),
            ("per_page".into(), "500".into()),
            ("footnote".into(), "y".into()),
        ];
        let value = self.http.get_json(&url, &params).await?;
        parse_world_bank(&value)
    }
}

pub fn parse_world_bank(value: &serde_json::Value) -> Result<Vec<WorldBankObservation>, DataError> {
    let rows = value
        .as_array()
        .and_then(|parts| parts.get(1))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| DataError::Parse {
            upstream: "world_bank".into(),
            message: "响应缺少第二段 observations 数组".into(),
        })?;
    Ok(rows
        .iter()
        .map(|row| WorldBankObservation {
            indicator_code: text_at(row, "/indicator/id"),
            indicator_name: text_at(row, "/indicator/value"),
            country_id: text_at(row, "/country/id"),
            country_iso3: text(row, "countryiso3code"),
            country_name: text_at(row, "/country/value"),
            period: text(row, "date"),
            value: row.get("value").and_then(serde_json::Value::as_f64),
            unit: text(row, "unit"),
            decimal_places: row
                .get("decimal")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
            observation_status: text(row, "obs_status"),
        })
        .collect())
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn text_at(value: &serde_json::Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_original_indicator_period_unit_and_country_code() {
        let value = serde_json::json!([
            {"page": 1, "pages": 1},
            [{"indicator": {"id": "NY.GDP.MKTP.CD", "value": "GDP (current US$)"},
              "country": {"id": "US", "value": "United States"}, "countryiso3code": "USA",
              "date": "2025", "value": 123.5, "unit": "USD", "obs_status": "", "decimal": 1}]
        ]);
        let rows = parse_world_bank(&value).unwrap();
        assert_eq!(rows[0].indicator_code, "NY.GDP.MKTP.CD");
        assert_eq!(rows[0].country_iso3, "USA");
        assert_eq!(rows[0].period, "2025");
        assert_eq!(rows[0].value, Some(123.5));
    }
}
