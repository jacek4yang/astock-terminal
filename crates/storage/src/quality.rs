//! Persistent data-quality observations, field lineage and reconciliation audit.

use std::collections::BTreeMap;

use astock_core::{
    AccountingScope, AdjustmentBasis, Currency, DataEnvelope, DataQualitySummary, DataUnit,
    DatasetKind, FreshnessPolicy, FreshnessState, QualityFlag, QualitySeverity,
    ReconciliationResult,
};
use serde::{Deserialize, Serialize};

use crate::{now_secs, Result, Storage};

fn token(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?.trim_matches('"').to_string())
}

fn parse_token<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    Ok(serde_json::from_str(&format!("\"{value}\""))?)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityObservation {
    pub observation_id: Option<i64>,
    pub dataset: DatasetKind,
    pub provider: String,
    pub entity_key: Option<String>,
    pub operation: String,
    pub success: bool,
    pub latency_ms: Option<u64>,
    pub summary: DataQualitySummary,
    pub missing_fields: u32,
    pub conflicts: u32,
    pub error_kind: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetSlo {
    pub dataset: DatasetKind,
    pub dataset_name: String,
    pub provider: String,
    pub observations: usize,
    pub successes: usize,
    pub error_rate: f64,
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub last_success_at: Option<i64>,
    pub consecutive_stale: usize,
    pub missing_fields: u64,
    pub conflicts: u64,
    pub current_freshness: FreshnessState,
    pub expected_cadence_secs: u64,
    pub stale_after_secs: u64,
    pub hard_expiry_secs: u64,
    pub latest_quality_flags: Vec<QualityFlag>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldLineageRecord {
    pub lineage_id: Option<i64>,
    pub dataset: DatasetKind,
    pub entity_key: String,
    pub field_path: String,
    pub source: String,
    pub source_url: Option<String>,
    pub event_time: Option<i64>,
    pub as_of_time: Option<i64>,
    pub publish_time: Option<i64>,
    pub fetched_at: i64,
    pub parser_version: String,
    pub schema_version: String,
    pub license: String,
    pub unit: Option<DataUnit>,
    pub currency: Option<Currency>,
    pub adjustment: AdjustmentBasis,
    pub revision: Option<String>,
    pub accounting_scope: AccountingScope,
    pub quality_flags: Vec<QualityFlag>,
    pub created_at: i64,
}

impl<T> From<(&str, &str, &DataEnvelope<T>)> for FieldLineageRecord {
    fn from((entity_key, field_path, envelope): (&str, &str, &DataEnvelope<T>)) -> Self {
        Self {
            lineage_id: None,
            dataset: envelope.dataset,
            entity_key: entity_key.into(),
            field_path: field_path.into(),
            source: envelope.source.clone(),
            source_url: envelope.source_url.clone(),
            event_time: envelope.event_time.map(|time| time.timestamp()),
            as_of_time: envelope.as_of_time.map(|time| time.timestamp()),
            publish_time: envelope.publish_time.map(|time| time.timestamp()),
            fetched_at: envelope.fetched_at.timestamp(),
            parser_version: envelope.parser_version.clone(),
            schema_version: envelope.schema_version.clone(),
            license: envelope.license.clone(),
            unit: envelope.unit,
            currency: envelope.currency,
            adjustment: envelope.adjustment,
            revision: envelope.revision.clone(),
            accounting_scope: envelope.accounting_scope,
            quality_flags: envelope.quality_flags.clone(),
            created_at: now_secs(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationAudit {
    pub reconciliation_id: Option<i64>,
    pub dataset: DatasetKind,
    pub entity_key: String,
    pub result: ReconciliationResult,
    pub blocking: bool,
    pub compared_at: i64,
}

impl Storage {
    pub async fn quality_observation_add(&self, observation: QualityObservation) -> Result<()> {
        let dataset = token(&observation.dataset)?;
        let freshness = token(&observation.summary.freshness)?;
        let flags = serde_json::to_string(&observation.summary.quality_flags)?;
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO data_quality_observations
                 (dataset,provider,entity_key,operation,success,latency_ms,freshness_state,
                  age_secs,expected_cadence_secs,stale_after_secs,hard_expiry_secs,
                  missing_fields,conflicts,quality_flags_json,error_kind,recorded_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                rusqlite::params![
                    dataset,
                    observation.provider,
                    observation.entity_key,
                    observation.operation,
                    observation.success,
                    observation.latency_ms.map(|value| value as i64),
                    freshness,
                    observation.summary.age_secs as i64,
                    observation.summary.expected_cadence_secs as i64,
                    observation.summary.stale_after_secs as i64,
                    observation.summary.hard_expiry_secs as i64,
                    observation.missing_fields as i64,
                    observation.conflicts as i64,
                    flags,
                    observation.error_kind,
                    observation.recorded_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn quality_observations_recent(
        &self,
        dataset: Option<DatasetKind>,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<QualityObservation>> {
        let dataset = dataset.map(|value| token(&value)).transpose()?;
        let provider = provider.map(str::to_string);
        let limit = limit.clamp(1, 5_000) as i64;
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT observation_id,dataset,provider,entity_key,operation,success,
                        latency_ms,freshness_state,age_secs,expected_cadence_secs,
                        stale_after_secs,hard_expiry_secs,missing_fields,conflicts,
                        quality_flags_json,error_kind,recorded_at
                 FROM data_quality_observations
                 WHERE (?1 IS NULL OR dataset=?1) AND (?2 IS NULL OR provider=?2)
                 ORDER BY recorded_at DESC,observation_id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(rusqlite::params![dataset, provider, limit], |row| {
                let dataset_text: String = row.get(1)?;
                let freshness_text: String = row.get(7)?;
                let flags_json: String = row.get(14)?;
                let dataset = parse_token(&dataset_text).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                let freshness = parse_token(&freshness_text).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                let quality_flags: Vec<QualityFlag> =
                    serde_json::from_str(&flags_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            14,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                let confidence_ceiling = if quality_flags
                    .iter()
                    .any(|flag: &QualityFlag| flag.severity == QualitySeverity::Blocking)
                {
                    astock_core::ConfidenceCeiling::Blocked
                } else if freshness == FreshnessState::Stale || !quality_flags.is_empty() {
                    astock_core::ConfidenceCeiling::Medium
                } else {
                    astock_core::ConfidenceCeiling::High
                };
                Ok(QualityObservation {
                    observation_id: row.get(0)?,
                    dataset,
                    provider: row.get(2)?,
                    entity_key: row.get(3)?,
                    operation: row.get(4)?,
                    success: row.get(5)?,
                    latency_ms: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
                    summary: DataQualitySummary {
                        dataset,
                        dataset_name: dataset.chinese_name().into(),
                        freshness,
                        age_secs: row.get::<_, i64>(8)? as u64,
                        expected_cadence_secs: row.get::<_, i64>(9)? as u64,
                        stale_after_secs: row.get::<_, i64>(10)? as u64,
                        hard_expiry_secs: row.get::<_, i64>(11)? as u64,
                        quality_flags,
                        confidence_ceiling,
                        allow_high_confidence: confidence_ceiling
                            == astock_core::ConfidenceCeiling::High,
                        allow_deterministic_compute: confidence_ceiling
                            != astock_core::ConfidenceCeiling::Blocked,
                    },
                    missing_fields: row.get::<_, i64>(12)? as u32,
                    conflicts: row.get::<_, i64>(13)? as u32,
                    error_kind: row.get(15)?,
                    recorded_at: row.get(16)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn quality_slo(&self, window_secs: u64) -> Result<Vec<DatasetSlo>> {
        let cutoff = now_secs().saturating_sub(window_secs.clamp(60, 31_536_000) as i64);
        let rows = self.quality_observations_recent(None, None, 5_000).await?;
        let mut grouped: BTreeMap<(String, String), Vec<QualityObservation>> = BTreeMap::new();
        for row in rows.into_iter().filter(|row| row.recorded_at >= cutoff) {
            grouped
                .entry((token(&row.dataset)?, row.provider.clone()))
                .or_default()
                .push(row);
        }
        let mut output = Vec::new();
        for ((_dataset_token, provider), mut rows) in grouped {
            rows.sort_by_key(|row| std::cmp::Reverse((row.recorded_at, row.observation_id)));
            let dataset = rows[0].dataset;
            let policy = FreshnessPolicy::for_dataset(dataset);
            let successes = rows.iter().filter(|row| row.success).count();
            let mut latencies = rows
                .iter()
                .filter_map(|row| row.latency_ms)
                .collect::<Vec<_>>();
            latencies.sort_unstable();
            let percentile = |values: &[u64], percentile: f64| {
                (!values.is_empty()).then(|| {
                    let index = ((values.len() - 1) as f64 * percentile).ceil() as usize;
                    values[index]
                })
            };
            let consecutive_stale = rows
                .iter()
                .take_while(|row| !row.success || row.summary.freshness != FreshnessState::Fresh)
                .count();
            output.push(DatasetSlo {
                dataset,
                dataset_name: dataset.chinese_name().into(),
                provider,
                observations: rows.len(),
                successes,
                error_rate: 1.0 - successes as f64 / rows.len() as f64,
                latency_p50_ms: percentile(&latencies, 0.50),
                latency_p95_ms: percentile(&latencies, 0.95),
                last_success_at: rows
                    .iter()
                    .filter(|row| row.success)
                    .map(|row| row.recorded_at)
                    .max(),
                consecutive_stale,
                missing_fields: rows.iter().map(|row| u64::from(row.missing_fields)).sum(),
                conflicts: rows.iter().map(|row| u64::from(row.conflicts)).sum(),
                current_freshness: if rows[0].success {
                    rows[0].summary.freshness
                } else {
                    FreshnessState::Stale
                },
                expected_cadence_secs: policy.expected_cadence_secs,
                stale_after_secs: policy.stale_after_secs,
                hard_expiry_secs: policy.hard_expiry_secs,
                latest_quality_flags: rows[0].summary.quality_flags.clone(),
            });
        }
        Ok(output)
    }

    pub async fn field_lineage_add(&self, record: FieldLineageRecord) -> Result<()> {
        let dataset = token(&record.dataset)?;
        let unit = record.unit.map(|value| token(&value)).transpose()?;
        let currency = record.currency.map(|value| token(&value)).transpose()?;
        let adjustment = token(&record.adjustment)?;
        let accounting_scope = token(&record.accounting_scope)?;
        let flags = serde_json::to_string(&record.quality_flags)?;
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO field_lineage_records
                 (dataset,entity_key,field_path,source,source_url,event_time,as_of_time,
                  publish_time,fetched_at,parser_version,schema_version,license,unit,currency,
                  adjustment,revision,accounting_scope,quality_flags_json,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                rusqlite::params![
                    dataset,
                    record.entity_key,
                    record.field_path,
                    record.source,
                    record.source_url,
                    record.event_time,
                    record.as_of_time,
                    record.publish_time,
                    record.fetched_at,
                    record.parser_version,
                    record.schema_version,
                    record.license,
                    unit,
                    currency,
                    adjustment,
                    record.revision,
                    accounting_scope,
                    flags,
                    record.created_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn field_lineage_recent(
        &self,
        dataset: Option<DatasetKind>,
        entity_key: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FieldLineageRecord>> {
        let dataset = dataset.map(|value| token(&value)).transpose()?;
        let entity_key = entity_key.map(str::to_string);
        let limit = limit.clamp(1, 5_000) as i64;
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT lineage_id,dataset,entity_key,field_path,source,source_url,event_time,
                        as_of_time,publish_time,fetched_at,parser_version,schema_version,license,
                        unit,currency,adjustment,revision,accounting_scope,quality_flags_json,created_at
                 FROM field_lineage_records WHERE (?1 IS NULL OR dataset=?1)
                   AND (?2 IS NULL OR entity_key=?2)
                 ORDER BY created_at DESC,lineage_id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(rusqlite::params![dataset,entity_key,limit], |row| {
                let dataset_text: String = row.get(1)?;
                let adjustment_text: String = row.get(15)?;
                let scope_text: String = row.get(17)?;
                let flags: String = row.get(18)?;
                Ok(FieldLineageRecord {
                    lineage_id: row.get(0)?,
                    dataset: parse_token(&dataset_text).map_err(|error| rusqlite::Error::FromSqlConversionFailure(1,rusqlite::types::Type::Text,Box::new(error)))?,
                    entity_key: row.get(2)?, field_path: row.get(3)?, source: row.get(4)?, source_url: row.get(5)?,
                    event_time: row.get(6)?, as_of_time: row.get(7)?, publish_time: row.get(8)?, fetched_at: row.get(9)?,
                    parser_version: row.get(10)?, schema_version: row.get(11)?, license: row.get(12)?,
                    unit: row.get::<_, Option<String>>(13)?.map(|value| parse_token::<DataUnit>(&value).map_err(|error| rusqlite::Error::FromSqlConversionFailure(13,rusqlite::types::Type::Text,Box::new(error)))).transpose()?,
                    currency: row.get::<_, Option<String>>(14)?.map(|value| parse_token::<Currency>(&value).map_err(|error| rusqlite::Error::FromSqlConversionFailure(14,rusqlite::types::Type::Text,Box::new(error)))).transpose()?,
                    adjustment: parse_token(&adjustment_text).map_err(|error| rusqlite::Error::FromSqlConversionFailure(15,rusqlite::types::Type::Text,Box::new(error)))?,
                    revision: row.get(16)?,
                    accounting_scope: parse_token(&scope_text).map_err(|error| rusqlite::Error::FromSqlConversionFailure(17,rusqlite::types::Type::Text,Box::new(error)))?,
                    quality_flags: serde_json::from_str(&flags).map_err(|error| rusqlite::Error::FromSqlConversionFailure(18,rusqlite::types::Type::Text,Box::new(error)))?,
                    created_at: row.get(19)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
        }).await
    }

    pub async fn reconciliation_add(&self, audit: ReconciliationAudit) -> Result<()> {
        let dataset = token(&audit.dataset)?;
        let status = token(&audit.result.status)?;
        let result_json = serde_json::to_string(&audit.result)?;
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO data_reconciliation_results
                 (dataset,entity_key,field_path,left_provider,right_provider,status,blocking,result_json,compared_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![dataset,audit.entity_key,audit.result.field,audit.result.left.provider,
                    audit.result.right.provider,status,audit.blocking,result_json,audit.compared_at],
            )?;
            Ok(())
        }).await
    }

    pub async fn reconciliations_recent(
        &self,
        dataset: Option<DatasetKind>,
        entity_key: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ReconciliationAudit>> {
        let dataset = dataset.map(|value| token(&value)).transpose()?;
        let entity_key = entity_key.map(str::to_string);
        let limit = limit.clamp(1, 1_000) as i64;
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT reconciliation_id,dataset,entity_key,blocking,result_json,compared_at
                 FROM data_reconciliation_results WHERE (?1 IS NULL OR dataset=?1)
                   AND (?2 IS NULL OR entity_key=?2)
                 ORDER BY compared_at DESC,reconciliation_id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(rusqlite::params![dataset, entity_key, limit], |row| {
                let dataset_text: String = row.get(1)?;
                let result_json: String = row.get(4)?;
                Ok(ReconciliationAudit {
                    reconciliation_id: row.get(0)?,
                    dataset: parse_token(&dataset_text).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    entity_key: row.get(2)?,
                    blocking: row.get(3)?,
                    result: serde_json::from_str(&result_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    compared_at: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StorageConfig;
    use astock_core::{ConfidenceCeiling, QualityFlagCode};

    #[tokio::test]
    async fn quality_slo_tracks_latency_errors_stale_missing_and_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        for (index, success) in [true, true, false].into_iter().enumerate() {
            let mut summary = DataQualitySummary::evaluate(
                DatasetKind::RealtimeQuote,
                if index == 0 { 1 } else { 60 },
                true,
                Vec::new(),
            );
            if !success {
                summary.confidence_ceiling = ConfidenceCeiling::Blocked;
            }
            storage
                .quality_observation_add(QualityObservation {
                    observation_id: None,
                    dataset: DatasetKind::RealtimeQuote,
                    provider: "fixture".into(),
                    entity_key: Some("600519".into()),
                    operation: "quote".into(),
                    success,
                    latency_ms: Some([10, 20, 100][index]),
                    summary,
                    missing_fields: index as u32,
                    conflicts: (!success) as u32,
                    error_kind: (!success).then(|| "network".into()),
                    recorded_at: now_secs() + index as i64,
                })
                .await
                .unwrap();
        }
        let slo = storage.quality_slo(86_400).await.unwrap();
        assert_eq!(slo.len(), 1);
        assert_eq!(slo[0].observations, 3);
        assert!(slo[0].error_rate > 0.3);
        assert_eq!(slo[0].latency_p95_ms, Some(100));
        assert_eq!(slo[0].missing_fields, 3);
        assert_eq!(slo[0].conflicts, 1);
        assert_eq!(slo[0].consecutive_stale, 2);
        assert_eq!(slo[0].current_freshness, FreshnessState::Stale);
    }

    #[tokio::test]
    async fn field_lineage_and_blocking_reconciliation_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let config = StorageConfig::with_base_dir(dir.path());
        let storage = Storage::open(config.clone()).unwrap();
        let envelope = DataEnvelope {
            data: 10.0,
            dataset: DatasetKind::RealtimeQuote,
            source: "tdx".into(),
            source_url: Some("https://example.com".into()),
            event_time: None,
            as_of_time: None,
            publish_time: None,
            fetched_at: astock_core::time::utc_now(),
            parser_version: "fixture".into(),
            schema_version: "1".into(),
            license: "fixture".into(),
            quality_flags: Vec::new(),
            unit: Some(DataUnit::Price),
            currency: Some(Currency::Cny),
            adjustment: AdjustmentBasis::None,
            revision: Some("r1".into()),
            accounting_scope: AccountingScope::NotApplicable,
        };
        storage
            .field_lineage_add(FieldLineageRecord::from(("600519", "price", &envelope)))
            .await
            .unwrap();
        let result = astock_core::reconcile_numeric(
            astock_core::NumericObservation {
                provider: "tdx".into(),
                field: "price".into(),
                value: 10.0,
                unit: DataUnit::Price,
                currency: Some(Currency::Cny),
                adjustment: AdjustmentBasis::None,
                accounting_scope: AccountingScope::NotApplicable,
                as_of_time: None,
            },
            astock_core::NumericObservation {
                provider: "eastmoney".into(),
                field: "price".into(),
                value: 11.0,
                unit: DataUnit::Price,
                currency: Some(Currency::Cny),
                adjustment: AdjustmentBasis::None,
                accounting_scope: AccountingScope::NotApplicable,
                as_of_time: None,
            },
            astock_core::ReconciliationTolerance {
                absolute: 0.01,
                relative: 0.002,
            },
        );
        storage
            .reconciliation_add(ReconciliationAudit {
                reconciliation_id: None,
                dataset: DatasetKind::RealtimeQuote,
                entity_key: "600519".into(),
                blocking: true,
                result,
                compared_at: now_secs(),
            })
            .await
            .unwrap();
        drop(storage);
        let reopened = Storage::open(config).unwrap();
        assert_eq!(
            reopened
                .field_lineage_recent(None, Some("600519"), 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let audits = reopened
            .reconciliations_recent(None, Some("600519"), 10)
            .await
            .unwrap();
        assert_eq!(audits.len(), 1);
        assert!(audits[0].blocking);
        assert!(audits[0]
            .result
            .quality_flags
            .iter()
            .any(|flag| flag.code == QualityFlagCode::SourceConflict));
    }
}
