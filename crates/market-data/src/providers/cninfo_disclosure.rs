//! CNInfo statutory disclosure index adapter.

use std::sync::Arc;

use astock_core::DataError;
use serde::{Deserialize, Serialize};

use crate::HttpClient;

const QUERY_URL: &str = "https://www.cninfo.com.cn/new/hisAnnouncement/query";
const STOCK_SEARCH_URL: &str = "https://www.cninfo.com.cn/new/information/topSearch/query";
const STATIC_BASE: &str = "https://static.cninfo.com.cn/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CninfoAnnouncement {
    pub announcement_id: String,
    pub title: String,
    pub announcement_time_ms: Option<i64>,
    pub security_code: String,
    pub security_name: String,
    pub org_id: String,
    pub category: String,
    pub pdf_url: String,
    pub adjunct_size_kb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CninfoPage {
    pub rows: Vec<CninfoAnnouncement>,
    pub total: u64,
    pub total_pages: u32,
    pub page: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CninfoStockIdentity {
    pub code: String,
    pub name: String,
    pub org_id: String,
    pub category: String,
    pub delisted: bool,
}

#[derive(Clone)]
pub struct CninfoDisclosureProvider {
    http: Arc<HttpClient>,
}

impl CninfoDisclosureProvider {
    pub fn new(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    /// Query CNInfo's public historical-announcement index. One request is
    /// made per page; callers own the bounded paging loop and cancellation.
    pub async fn query(
        &self,
        stock_code: Option<&str>,
        market_column: &str,
        page: u32,
        page_size: u32,
        date_range: Option<&str>,
    ) -> Result<CninfoPage, DataError> {
        let page = page.max(1);
        // The public front-end contract currently caps a page at 30 rows even
        // when a larger value is requested. Use the real limit so page counts
        // and completeness diagnostics remain truthful.
        let page_size = page_size.clamp(10, 30);
        let form = vec![
            ("pageNum".into(), page.to_string()),
            ("pageSize".into(), page_size.to_string()),
            ("column".into(), market_column.to_string()),
            ("tabName".into(), "fulltext".into()),
            ("plate".into(), String::new()),
            ("stock".into(), stock_code.unwrap_or_default().to_string()),
            ("searchkey".into(), String::new()),
            ("secid".into(), String::new()),
            ("category".into(), String::new()),
            ("trade".into(), String::new()),
            ("seDate".into(), date_range.unwrap_or_default().to_string()),
            ("sortName".into(), String::new()),
            ("sortType".into(), String::new()),
            ("isHLtitle".into(), "true".into()),
        ];
        let headers = vec![("Referer".into(), "https://www.cninfo.com.cn/".into())];
        let value = self.http.post_form_json(QUERY_URL, &headers, &form).await?;
        parse_cninfo_page(&value, page)
    }

    /// Resolve the CNInfo-specific `orgId`. The announcement endpoint does
    /// not accept a bare six-digit code: its `stock` form field must be
    /// `<code>,<orgId>`. The legacy implementation passed only the code and
    /// silently returned zero rows for active issuers.
    pub async fn resolve_stock_identity(
        &self,
        security_code: &str,
    ) -> Result<CninfoStockIdentity, DataError> {
        let headers = vec![
            ("Referer".into(), "https://www.cninfo.com.cn/".into()),
            ("Origin".into(), "https://www.cninfo.com.cn".into()),
            ("X-Requested-With".into(), "XMLHttpRequest".into()),
        ];
        let form = vec![
            ("keyWord".into(), security_code.to_string()),
            ("maxNum".into(), "10".into()),
        ];
        let value = self
            .http
            .post_form_json(STOCK_SEARCH_URL, &headers, &form)
            .await?;
        parse_cninfo_stock_identity(&value, security_code)
            .ok_or_else(|| DataError::Empty(format!("CNInfo orgId lookup for {security_code}")))
    }

    /// Query a bounded recent window for one security, resolving `orgId`
    /// first and paging with CNInfo's actual 30-row limit.
    pub async fn query_recent_for_stock(
        &self,
        security_code: &str,
        market_column: &str,
        date_range: Option<&str>,
        max_pages: u32,
    ) -> Result<CninfoPage, DataError> {
        let identity = self.resolve_stock_identity(security_code).await?;
        let stock = format!("{},{}", identity.code, identity.org_id);
        let first = self
            .query(Some(&stock), market_column, 1, 30, date_range)
            .await?;
        let total = first.total;
        let total_pages = first.total_pages;
        let mut rows = first.rows;
        let last = total_pages.min(max_pages.max(1));
        for page in 2..=last {
            let next = self
                .query(Some(&stock), market_column, page, 30, date_range)
                .await?;
            rows.extend(next.rows);
        }
        let mut seen = std::collections::HashSet::new();
        rows.retain(|row| {
            let key = if row.announcement_id.is_empty() {
                format!(
                    "{}:{:?}:{}",
                    row.security_code, row.announcement_time_ms, row.title
                )
            } else {
                row.announcement_id.clone()
            };
            seen.insert(key)
        });
        Ok(CninfoPage {
            rows,
            total,
            total_pages,
            page: 1,
        })
    }
}

fn parse_cninfo_stock_identity(
    value: &serde_json::Value,
    security_code: &str,
) -> Option<CninfoStockIdentity> {
    value.as_array()?.iter().find_map(|row| {
        let code = text(row, "code");
        let org_id = text(row, "orgId");
        if code != security_code || org_id.is_empty() {
            return None;
        }
        Some(CninfoStockIdentity {
            code,
            name: text(row, "zwjc"),
            org_id,
            category: text(row, "category"),
            delisted: text(row, "delisted").eq_ignore_ascii_case("true"),
        })
    })
}

pub fn parse_cninfo_page(value: &serde_json::Value, page: u32) -> Result<CninfoPage, DataError> {
    let announcements = value
        .get("announcements")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rows = Vec::with_capacity(announcements.len());
    for row in announcements {
        let path = text(&row, "adjunctUrl");
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path
        } else {
            format!("{STATIC_BASE}{}", path.trim_start_matches('/'))
        };
        let title = strip_highlight(&text(&row, "announcementTitle"));
        if title.is_empty() || url == STATIC_BASE {
            continue;
        }
        rows.push(CninfoAnnouncement {
            announcement_id: text(&row, "announcementId"),
            title,
            announcement_time_ms: row
                .get("announcementTime")
                .and_then(serde_json::Value::as_i64),
            security_code: text(&row, "secCode"),
            security_name: text(&row, "secName"),
            org_id: text(&row, "orgId"),
            category: text(&row, "announcementTypeName"),
            pdf_url: url,
            adjunct_size_kb: row.get("adjunctSize").and_then(serde_json::Value::as_u64),
        });
    }
    let total = value
        .get("totalAnnouncement")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(rows.len() as u64);
    let total_pages = value
        .get("totalpages")
        .or_else(|| value.get("totalPages"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1) as u32;
    Ok(CninfoPage {
        rows,
        total,
        total_pages,
        page,
    })
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn strip_highlight(value: &str) -> String {
    value
        .replace("<em>", "")
        .replace("</em>", "")
        .replace("<span class='keyword'>", "")
        .replace("</span>", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_statutory_pdf_and_preserves_multi_security_fields() {
        let value = serde_json::json!({
            "totalAnnouncement": 1, "totalpages": 1,
            "announcements": [{
                "announcementId": "121234", "announcementTitle": "<em>年度报告</em>",
                "announcementTime": 1750000000000_i64, "secCode": "000001", "secName": "平安银行",
                "orgId": "gssz0000001", "announcementTypeName": "年度报告",
                "adjunctUrl": "finalpage/2026-01-01/a.PDF", "adjunctSize": 1024
            }]
        });
        let page = parse_cninfo_page(&value, 1).unwrap();
        assert_eq!(page.rows[0].title, "年度报告");
        assert_eq!(
            page.rows[0].pdf_url,
            "https://static.cninfo.com.cn/finalpage/2026-01-01/a.PDF"
        );
        assert_eq!(page.rows[0].security_code, "000001");
    }

    #[test]
    fn parses_exact_cninfo_stock_identity() {
        let value = serde_json::json!([{
            "code": "000725", "zwjc": "京东方Ａ", "orgId": "gssz0000725",
            "category": "A股", "delisted": "false"
        }, {
            "code": "200725", "zwjc": "京东方Ｂ", "orgId": "gssz0000725",
            "category": "B股", "delisted": "false"
        }]);
        let identity = parse_cninfo_stock_identity(&value, "000725").unwrap();
        assert_eq!(identity.code, "000725");
        assert_eq!(identity.org_id, "gssz0000725");
        assert!(!identity.delisted);
    }
}
