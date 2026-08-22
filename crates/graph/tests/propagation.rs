//! End-to-end integration: seed the built-in graph, propagate
//! "铜价上涨10%", and verify the expected 受益/受损 companies appear at the
//! correct hop levels with readable logic chains (snapshot-style).

use astock_graph::{seed_if_empty, Engine, Event, GraphStore, ImpactDirection, ImpactReport};
use astock_storage::{Storage, StorageConfig};

fn codes(entries: &[astock_graph::ImpactEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.code.as_str()).collect()
}

fn find<'a>(report: &'a ImpactReport, code: &str) -> &'a astock_graph::ImpactEntry {
    report
        .primary_benefit
        .iter()
        .chain(&report.primary_harm)
        .chain(&report.secondary_benefit)
        .chain(&report.secondary_harm)
        .chain(&report.potential)
        .find(|e| e.code == code)
        .unwrap_or_else(|| panic!("{code} not impacted"))
}

#[tokio::test]
async fn seed_then_propagate_copper_price_up() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let store = GraphStore::new(storage);

    let summary = seed_if_empty(&store).await.unwrap();
    assert!(!summary.skipped);
    assert!(summary.nodes >= 60, "nodes: {}", summary.nodes);
    assert!(summary.edges >= 120, "edges: {}", summary.edges);

    let engine = Engine::new(store.clone());
    let event = Event::new(
        "evt-cu-1",
        "commodity_price",
        "铜价上涨10%",
        "commodity:copper",
        Some(0.10),
        1,
        1_700_000_000,
    );
    // Persist the event, then propagate.
    store.insert_event(&event).await.unwrap();
    let report = engine.propagate(&event).await.unwrap();

    // 一级受益: copper miners (produces).
    for code in ["601899", "600362", "000878", "000630"] {
        assert!(
            codes(&report.primary_benefit).contains(&code),
            "{code} missing from 一级受益: {:?}",
            codes(&report.primary_benefit)
        );
    }
    // 一级受损: cable/grid-equipment makers (consumes).
    for code in ["600869", "002533", "601179", "600312", "600089"] {
        assert!(
            codes(&report.primary_harm).contains(&code),
            "{code} missing from 一级受损: {:?}",
            codes(&report.primary_harm)
        );
    }
    // 二级受益: aluminum producers (substitutes → 替代受益).
    for code in ["601600", "600219"] {
        assert!(
            codes(&report.secondary_benefit).contains(&code),
            "{code} missing from 二级受益: {:?}",
            codes(&report.secondary_benefit)
        );
    }
    // 二级受损: appliance makers via cost pass-through (consumes 线缆).
    for code in ["000651", "000333", "600690"] {
        assert!(
            codes(&report.secondary_harm).contains(&code),
            "{code} missing from 二级受损: {:?}",
            codes(&report.secondary_harm)
        );
    }

    // Hop levels and directions.
    let jiangxi = find(&report, "600362");
    assert_eq!(jiangxi.hop, 1);
    assert_eq!(jiangxi.direction, ImpactDirection::Benefit);
    let gree = find(&report, "000651");
    assert_eq!(gree.hop, 2);
    assert_eq!(gree.direction, ImpactDirection::Harm);
    let chalco = find(&report, "601600");
    assert_eq!(chalco.hop, 2);
    assert_eq!(chalco.direction, ImpactDirection::Benefit);

    // Snapshot the logic chains (readable, deterministic).
    assert_eq!(
        jiangxi.logic_chain,
        "铜↑10% → 江西铜业（自产铜，受益）"
    );
    assert_eq!(
        gree.logic_chain,
        "铜↑10% → 远东股份（采购铜，成本上升，受损） → 电线电缆（成本传导提价） → 格力电器（采购电线电缆，成本上升，受损）"
    );
    assert_eq!(
        chalco.logic_chain,
        "铜↑10% → 铝（铜的替代品，需求转移） → 中国铝业（自产铝，受益）"
    );

    // Confidence decays per hop: every hop-2 entry < its hop-1 path start.
    assert!(gree.confidence < find(&report, "600869").confidence);
    // Magnitude estimates are bounded by the event magnitude (weights <= 1).
    assert!(gree.magnitude_estimate.unwrap() <= 0.10);
    assert!(gree.magnitude_estimate.unwrap() > 0.0);
    // Lag heuristic: miner reprices fast, appliance maker waits for the
    // two-step cost pass-through.
    assert_eq!(jiangxi.expected_lag_days, 5);
    assert_eq!(gree.expected_lag_days, 65);

    // Provenance travels with every entry.
    assert!(!jiangxi.provenance.is_empty());
    assert!(jiangxi.provenance.iter().all(|(name, url)| !name.is_empty() && !url.is_empty()));

    // Ranking: primary buckets before secondary; confidence descending
    // within a bucket.
    let confs: Vec<f64> = report.primary_benefit.iter().map(|e| e.confidence).collect();
    assert!(confs.windows(2).all(|w| w[0] >= w[1]));

    // The disclaimer labels everything as heuristic.
    assert!(report.disclaimer.contains("粗略启发式估计"));
    assert!(report.summary.contains("一级受益"));
}

#[tokio::test]
async fn propagate_lithium_down_via_name_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let store = GraphStore::new(storage);
    seed_if_empty(&store).await.unwrap();

    let engine = Engine::new(store);
    // Subject resolved by Chinese name, direction down.
    let event = Event::new("evt-li-1", "commodity_price", "碳酸锂下跌20%", "碳酸锂", Some(0.20), -1, 0);
    let report = engine.propagate(&event).await.unwrap();

    // Falling lithium carbonate: miners hurt, cathode makers helped.
    assert!(codes(&report.primary_harm).contains(&"002460")); // 赣锋锂业
    assert!(codes(&report.primary_harm).contains(&"002466")); // 天齐锂业
    assert!(codes(&report.primary_benefit).contains(&"300073")); // 当升科技
    assert!(codes(&report.primary_benefit).contains(&"688005")); // 容百科技
    // 二阶: cathode price follows → battery makers' cost falls (受益).
    assert!(codes(&report.secondary_benefit).contains(&"300750")); // 宁德时代
}

#[tokio::test]
async fn propagate_company_accident_hits_suppliers_customers_competitors() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let store = GraphStore::new(storage);
    seed_if_empty(&store).await.unwrap();

    let engine = Engine::new(store);
    // 格力电器事故 (negative company event): competitors gain.
    let event = Event::new("evt-gree", "accident", "格力电器工厂事故", "000651", None, -1, 0);
    let report = engine.propagate(&event).await.unwrap();

    // Competitor 美的集团 benefits at hop 1 (competes, company subject).
    let midea = find(&report, "000333");
    assert_eq!(midea.hop, 1);
    assert_eq!(midea.direction, ImpactDirection::Benefit);
    assert!(midea.logic_chain.contains("竞争"));
    // No quantified magnitude → no estimate, still a full chain.
    assert!(midea.magnitude_estimate.is_none());
}
