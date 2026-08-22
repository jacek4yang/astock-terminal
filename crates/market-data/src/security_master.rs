//! In-memory canonical security master populated from exchange-wide lists.

use astock_core::{SearchResult, SecurityMasterRecord, StockListItem};
use dashmap::DashMap;

/// Thread-safe security identity index shared by quote, search, graph and UI.
pub struct SecurityMaster {
    records: DashMap<String, SecurityMasterRecord>,
}

impl Default for SecurityMaster {
    fn default() -> Self {
        let master = Self {
            records: DashMap::new(),
        };
        // Offline-safe regression identities. Provider refreshes replace these
        // and populate the complete universe; this is never the sole dataset.
        for (code, name) in [
            ("300308", "中际旭创"),
            ("000001", "平安银行"),
            ("600519", "贵州茅台"),
            ("300750", "宁德时代"),
            ("688981", "中芯国际"),
        ] {
            master.upsert(SecurityMasterRecord::listed_stock(
                code,
                name,
                "offline_regression_seed",
            ));
        }
        master
    }
}

impl SecurityMaster {
    /// Insert or replace a canonical record.
    pub fn upsert(&self, record: SecurityMasterRecord) {
        if record.code.len() == 6 && !record.canonical_name.trim().is_empty() {
            self.records.insert(record.code.clone(), record);
        }
    }

    /// Merge a provider-wide stock list into the identity index.
    pub fn merge_stock_list(&self, items: &[StockListItem], source: &str) {
        for item in items {
            if !item.name.trim().is_empty() {
                self.upsert(SecurityMasterRecord::listed_stock(
                    item.code.clone(),
                    item.name.clone(),
                    source,
                ));
            }
        }
    }

    /// Merge records loaded from durable storage.
    pub fn merge_records(&self, records: impl IntoIterator<Item = SecurityMasterRecord>) {
        for record in records {
            self.upsert(record);
        }
    }

    /// Resolve one six-digit code.
    pub fn get(&self, code: &str) -> Option<SecurityMasterRecord> {
        self.records.get(code).map(|record| record.clone())
    }

    /// Search canonical names, codes and aliases without a network dependency.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<_> = self
            .records
            .iter()
            .filter(|entry| {
                entry.code.contains(&query)
                    || entry.canonical_name.to_lowercase().contains(&query)
                    || entry
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(&query))
            })
            .map(|entry| SearchResult {
                code: entry.code.clone(),
                name: entry.canonical_name.clone(),
                classify: format!("{:?}", entry.board).to_lowercase(),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.code
                .starts_with(&query)
                .cmp(&a.code.starts_with(&query))
                .then_with(|| a.code.cmp(&b.code))
        });
        rows.truncate(limit);
        rows
    }

    /// Snapshot all records for persistence and diagnostics.
    pub fn all(&self) -> Vec<SecurityMasterRecord> {
        let mut rows: Vec<_> = self.records.iter().map(|entry| entry.clone()).collect();
        rows.sort_by(|a, b| a.code.cmp(&b.code));
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_identities_work_offline() {
        let master = SecurityMaster::default();
        assert_eq!(master.get("300308").unwrap().canonical_name, "中际旭创");
        assert_eq!(master.search("平安", 10)[0].code, "000001");
    }
}
