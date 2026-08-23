//! U.S. SEC EDGAR submissions adapter.
//!
//! Automated access is disabled until the user supplies a Fair Access
//! User-Agent (`ASTOCK_SEC_USER_AGENT`) containing their application identity
//! and contact. The adapter never sends an invented contact address.

use std::sync::Arc;

use astock_core::DataError;
use serde::{Deserialize, Serialize};

use crate::HttpClient;

const SUBMISSIONS_BASE: &str = "https://data.sec.gov/submissions";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecFiling {
    pub cik: String,
    pub legal_name: String,
    pub tickers: Vec<String>,
    pub exchanges: Vec<String>,
    pub accession_number: String,
    pub form: String,
    pub filing_date: String,
    pub report_date: String,
    pub acceptance_datetime: String,
    pub primary_document: String,
    pub primary_document_url: String,
    pub file_number: String,
    pub items: String,
    pub size_bytes: Option<u64>,
    pub is_xbrl: bool,
    pub is_inline_xbrl: bool,
}

#[derive(Clone)]
pub struct SecEdgarProvider {
    http: Arc<HttpClient>,
}

impl SecEdgarProvider {
    pub fn new(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    pub async fn submissions(&self, cik: &str) -> Result<Vec<SecFiling>, DataError> {
        let digits: String = cik.chars().filter(char::is_ascii_digit).collect();
        if digits.is_empty() || digits.len() > 10 {
            return Err(DataError::Parse {
                upstream: "sec_edgar".into(),
                message: "CIK 必须为 1 至 10 位数字".into(),
            });
        }
        let cik = format!("{digits:0>10}");
        let user_agent = std::env::var("ASTOCK_SEC_USER_AGENT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(DataError::NoProvider(
                "sec_edgar_missing_ASTOCK_SEC_USER_AGENT",
            ))?;
        let url = format!("{SUBMISSIONS_BASE}/CIK{cik}.json");
        let headers = vec![
            ("User-Agent".into(), user_agent),
            ("Accept-Encoding".into(), "gzip, deflate".into()),
            ("Host".into(), "data.sec.gov".into()),
        ];
        let value = self.http.get_json_with_headers(&url, &headers, &[]).await?;
        parse_sec_submissions(&value)
    }
}

pub fn parse_sec_submissions(value: &serde_json::Value) -> Result<Vec<SecFiling>, DataError> {
    let cik = value
        .get("cik")
        .map(value_text)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DataError::Parse {
            upstream: "sec_edgar".into(),
            message: "missing cik".into(),
        })?;
    let legal_name = value.get("name").map(value_text).unwrap_or_default();
    let tickers = string_array(value.get("tickers"));
    let exchanges = string_array(value.get("exchanges"));
    let recent = value
        .pointer("/filings/recent")
        .ok_or_else(|| DataError::Parse {
            upstream: "sec_edgar".into(),
            message: "missing filings.recent".into(),
        })?;
    let accessions = string_array(recent.get("accessionNumber"));
    let forms = string_array(recent.get("form"));
    let filing_dates = string_array(recent.get("filingDate"));
    let report_dates = string_array(recent.get("reportDate"));
    let acceptance = string_array(recent.get("acceptanceDateTime"));
    let primary_documents = string_array(recent.get("primaryDocument"));
    let file_numbers = string_array(recent.get("fileNumber"));
    let items = string_array(recent.get("items"));
    let sizes = u64_array(recent.get("size"));
    let xbrl = boolish_array(recent.get("isXBRL"));
    let inline_xbrl = boolish_array(recent.get("isInlineXBRL"));
    let cik_number = cik.trim_start_matches('0');
    let mut output = Vec::with_capacity(accessions.len());
    for (index, accession) in accessions.into_iter().enumerate() {
        let document = primary_documents.get(index).cloned().unwrap_or_default();
        if accession.is_empty() || document.is_empty() {
            continue;
        }
        let compact_accession = accession.replace('-', "");
        output.push(SecFiling {
            cik: format!("{cik:0>10}"),
            legal_name: legal_name.clone(),
            tickers: tickers.clone(),
            exchanges: exchanges.clone(),
            accession_number: accession,
            form: forms.get(index).cloned().unwrap_or_default(),
            filing_date: filing_dates.get(index).cloned().unwrap_or_default(),
            report_date: report_dates.get(index).cloned().unwrap_or_default(),
            acceptance_datetime: acceptance.get(index).cloned().unwrap_or_default(),
            primary_document_url: format!(
                "https://www.sec.gov/Archives/edgar/data/{cik_number}/{compact_accession}/{document}"
            ),
            primary_document: document,
            file_number: file_numbers.get(index).cloned().unwrap_or_default(),
            items: items.get(index).cloned().unwrap_or_default(),
            size_bytes: sizes.get(index).copied().flatten(),
            is_xbrl: xbrl.get(index).copied().unwrap_or(false),
            is_inline_xbrl: inline_xbrl.get(index).copied().unwrap_or(false),
        });
    }
    Ok(output)
}

fn value_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string().trim_matches('"').to_string())
}

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|values| values.iter().map(value_text).collect())
        .unwrap_or_default()
}

fn u64_array(value: Option<&serde_json::Value>) -> Vec<Option<u64>> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|values| values.iter().map(serde_json::Value::as_u64).collect())
        .unwrap_or_default()
}

fn boolish_array(value: Option<&serde_json::Value>) -> Vec<bool> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_bool().unwrap_or_else(|| value.as_i64() == Some(1)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_columnar_submission_and_builds_official_archive_url() {
        let value = serde_json::json!({
            "cik": "320193", "name": "Apple Inc.", "tickers": ["AAPL"], "exchanges": ["Nasdaq"],
            "filings": { "recent": {
                "accessionNumber": ["0000320193-26-000001"], "form": ["10-Q"],
                "filingDate": ["2026-08-01"], "reportDate": ["2026-06-30"],
                "acceptanceDateTime": ["2026-08-01T16:05:00.000Z"],
                "primaryDocument": ["aapl-20260630.htm"], "fileNumber": ["001-36743"],
                "items": [""], "size": [12345], "isXBRL": [1], "isInlineXBRL": [1]
            }}
        });
        let rows = parse_sec_submissions(&value).unwrap();
        assert_eq!(rows[0].cik, "0000320193");
        assert_eq!(rows[0].form, "10-Q");
        assert_eq!(rows[0].tickers, vec!["AAPL"]);
        assert_eq!(
            rows[0].primary_document_url,
            "https://www.sec.gov/Archives/edgar/data/320193/000032019326000001/aapl-20260630.htm"
        );
        assert!(rows[0].is_xbrl);
    }
}
