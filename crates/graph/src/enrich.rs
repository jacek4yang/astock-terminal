//! Industry-classification enrichment from live EastMoney data.
//!
//! [`enrich_from_eastmoney`] pulls the full A-share list with EM industry
//! tags (clist `f100`) and creates `belongs_to` edges from every matching
//! company node to an industry node (`industry:em:{tag}`, created on
//! demand). All such edges carry provenance `东方财富行业分类` and a fixed
//! confidence of `0.75` (public classification data, not a filing).
//!
//! The write path is [`apply_industry_map`], which is pure with respect to
//! the network and therefore unit-testable.

use astock_market_data::{IndustryClassified, MarketData};

use crate::error::Result;
use crate::model::{Edge, Node, NodeKind, Relation};
use crate::store::{now_secs, GraphStore};

/// Provenance source name attached to every enriched edge.
pub const EM_INDUSTRY_SOURCE: &str = "东方财富行业分类";
/// Provenance URL for EM industry classification.
pub const EM_INDUSTRY_URL: &str = "https://quote.eastmoney.com/center/gridlist.html";
/// Fixed confidence for EM classification edges (public data tier).
pub const EM_INDUSTRY_CONFIDENCE: f64 = 0.75;

/// Industry node id for an EastMoney industry tag. The `em:` prefix keeps
/// EM's taxonomy separate from the curated seed industries.
pub fn industry_node_id(industry: &str) -> String {
    format!("industry:em:{industry}")
}

/// Summary of an enrichment run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichSummary {
    /// Company nodes matched against the classification list.
    pub companies_matched: usize,
    /// Industry nodes created (already-existing ones are not counted
    /// precisely — upserts are idempotent, so this is a lower bound of 0).
    pub edges_upserted: usize,
    /// Company codes present in the classification list but absent from
    /// the graph (informational only).
    pub unmatched_listings: usize,
}

/// Apply a classification list to the graph: for every company node whose
/// code appears in `items`, ensure the EM industry node exists and upsert
/// a `belongs_to` edge. Idempotent (all writes are upserts).
pub async fn apply_industry_map(
    store: &GraphStore,
    items: &[IndustryClassified],
) -> Result<EnrichSummary> {
    let mut summary = EnrichSummary::default();
    let nodes = store.all_nodes().await?;
    let companies: Vec<&Node> = nodes.iter().filter(|n| n.kind == NodeKind::Company).collect();

    for item in items {
        let Some(company) = companies.iter().find(|n| n.code.as_deref() == Some(item.code.as_str()))
        else {
            summary.unmatched_listings += 1;
            continue;
        };
        let industry_id = industry_node_id(&item.industry);
        if store.node(&industry_id).await?.is_none() {
            store
                .upsert_node(&Node {
                    id: industry_id.clone(),
                    kind: NodeKind::Industry,
                    name: item.industry.clone(),
                    code: None,
                    meta: serde_json::json!({"source": EM_INDUSTRY_SOURCE}),
                })
                .await?;
        }
        store
            .upsert_edge(&Edge {
                id: None,
                src: company.id.clone(),
                dst: industry_id,
                relation: Relation::BelongsTo,
                weight: 1.0,
                source_name: EM_INDUSTRY_SOURCE.to_string(),
                source_url: EM_INDUSTRY_URL.to_string(),
                confidence: EM_INDUSTRY_CONFIDENCE,
                valid_from: now_secs(),
                valid_to: None,
            })
            .await?;
        summary.companies_matched += 1;
        summary.edges_upserted += 1;
    }
    Ok(summary)
}

/// Fetch the live EM industry classification and apply it to the graph.
pub async fn enrich_from_eastmoney(
    store: &GraphStore,
    market: &MarketData,
) -> Result<EnrichSummary> {
    let fetched = market.eastmoney.industry_map().await?;
    let summary = apply_industry_map(store, &fetched.data).await?;
    tracing::info!(
        matched = summary.companies_matched,
        edges = summary.edges_upserted,
        "industry enrichment applied"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::seed_if_empty;
    use astock_storage::{Storage, StorageConfig};

    fn item(code: &str, name: &str, industry: &str) -> IndustryClassified {
        IndustryClassified {
            code: code.into(),
            name: name.into(),
            industry: industry.into(),
        }
    }

    #[tokio::test]
    async fn apply_industry_map_creates_belongs_to_edges() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = GraphStore::new(storage);
        seed_if_empty(&store).await.unwrap();

        let items = vec![
            item("600519", "贵州茅台", "酿酒行业"),
            item("600362", "江西铜业", "有色金属"),
            item("999999", "不存在", "幽灵行业"), // not in the graph
        ];
        let summary = apply_industry_map(&store, &items).await.unwrap();
        assert_eq!(summary.companies_matched, 2);
        assert_eq!(summary.edges_upserted, 2);
        assert_eq!(summary.unmatched_listings, 1);

        // Industry node created with EM provenance, edge attached.
        let industry = store
            .node(&industry_node_id("酿酒行业"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(industry.kind, NodeKind::Industry);
        let neighbors = store.neighbors("company:600519").await.unwrap();
        let em_edge = neighbors
            .iter()
            .find(|(e, _)| e.dst == industry_node_id("酿酒行业"))
            .map(|(e, _)| e)
            .unwrap();
        assert_eq!(em_edge.relation, Relation::BelongsTo);
        assert_eq!(em_edge.source_name, EM_INDUSTRY_SOURCE);
        assert_eq!(em_edge.confidence, EM_INDUSTRY_CONFIDENCE);

        // Idempotent: a second run upserts the same edges, no growth.
        let before = store.all_edges().await.unwrap().len();
        let again = apply_industry_map(&store, &items).await.unwrap();
        assert_eq!(again.edges_upserted, 2);
        assert_eq!(store.all_edges().await.unwrap().len(), before);
    }
}
