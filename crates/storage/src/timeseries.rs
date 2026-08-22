//! Parquet time-series cache.
//!
//! Kline bars are stored partitioned as
//! `timeseries/{symbol}/{period}/{adjust}.parquet`, fund flow as
//! `timeseries/fund_flow/{symbol}.parquet`. All reads/writes are plain
//! synchronous filesystem IO; [`crate::Storage`] wraps them in async methods.
//!
//! The row types ([`BarRow`], [`FundFlowRow`]) are deliberately local to this
//! crate (they carry storage-level provenance); they mirror the core domain
//! shapes, so conversion to `astock-core` types is a field-by-field map at
//! the call sites that need it (see `Storage::load_bars_adjusted`).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray,
};
use arrow::datatypes::{DataType, Field, Schema};
use chrono::{Datelike, NaiveDate};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriter;

use crate::error::{Error, Result};

/// Days between the chrono epoch base and the unix epoch.
const UNIX_EPOCH_DAYS_FROM_CE: i32 = 719_163;

/// One kline bar with fetch provenance.
///
/// Mirrors the core `Bar` shape (`date`, OHLC, `volume`, optional `amount` /
/// `turnover`) plus provenance fields; `date` is a `chrono::NaiveDate` and
/// `fetched_at` is unix seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct BarRow {
    /// Trading date.
    pub date: NaiveDate,
    /// Open price.
    pub open: f64,
    /// High price.
    pub high: f64,
    /// Low price.
    pub low: f64,
    /// Close price.
    pub close: f64,
    /// Volume (shares).
    pub volume: f64,
    /// Turnover amount (CNY) if the source provides it.
    pub amount: Option<f64>,
    /// Turnover rate if the source provides it.
    pub turnover: Option<f64>,
    /// Data source label, e.g. "eastmoney".
    pub source: String,
    /// Fetch time, unix seconds.
    pub fetched_at: i64,
}

/// One fund-flow observation (net inflows by order size bucket, CNY).
#[derive(Debug, Clone, PartialEq)]
pub struct FundFlowRow {
    /// Trading date.
    pub date: NaiveDate,
    /// Main-force net inflow.
    pub main_net_inflow: Option<f64>,
    /// Super-large order net inflow.
    pub super_large: Option<f64>,
    /// Large order net inflow.
    pub large: Option<f64>,
    /// Medium order net inflow.
    pub medium: Option<f64>,
    /// Small order net inflow.
    pub small: Option<f64>,
    /// Data source label.
    pub source: String,
    /// Fetch time, unix seconds.
    pub fetched_at: i64,
}

fn bar_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("date", DataType::Date32, false),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
        Field::new("amount", DataType::Float64, true),
        Field::new("turnover", DataType::Float64, true),
        Field::new("source", DataType::Utf8, false),
        Field::new("fetched_at", DataType::Int64, false),
    ]))
}

fn fund_flow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("date", DataType::Date32, false),
        Field::new("main_net_inflow", DataType::Float64, true),
        Field::new("super_large", DataType::Float64, true),
        Field::new("large", DataType::Float64, true),
        Field::new("medium", DataType::Float64, true),
        Field::new("small", DataType::Float64, true),
        Field::new("source", DataType::Utf8, false),
        Field::new("fetched_at", DataType::Int64, false),
    ]))
}

fn date_to_days(date: NaiveDate) -> i32 {
    date.num_days_from_ce() - UNIX_EPOCH_DAYS_FROM_CE
}

fn days_to_date(days: i32) -> Result<NaiveDate> {
    NaiveDate::from_num_days_from_ce_opt(days + UNIX_EPOCH_DAYS_FROM_CE)
        .ok_or_else(|| Error::Invalid(format!("date32 value out of range: {days}")))
}

fn bars_to_batch(bars: &[BarRow]) -> Result<RecordBatch> {
    let batch = RecordBatch::try_new(
        bar_schema(),
        vec![
            Arc::new(Date32Array::from(
                bars.iter()
                    .map(|b| date_to_days(b.date))
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.open).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.high).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.low).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.close).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.volume).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.amount).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                bars.iter().map(|b| b.turnover).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                bars.iter().map(|b| b.source.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                bars.iter().map(|b| b.fetched_at).collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

fn batch_to_bars(batch: &RecordBatch) -> Result<Vec<BarRow>> {
    fn col<T: 'static>(batch: &RecordBatch, idx: usize) -> Result<&T> {
        batch
            .column(idx)
            .as_any()
            .downcast_ref::<T>()
            .ok_or_else(|| Error::Invalid(format!("unexpected parquet column type at {idx}")))
    }
    let dates = col::<Date32Array>(batch, 0)?;
    let open = col::<Float64Array>(batch, 1)?;
    let high = col::<Float64Array>(batch, 2)?;
    let low = col::<Float64Array>(batch, 3)?;
    let close = col::<Float64Array>(batch, 4)?;
    let volume = col::<Float64Array>(batch, 5)?;
    let amount = col::<Float64Array>(batch, 6)?;
    let turnover = col::<Float64Array>(batch, 7)?;
    let source = col::<StringArray>(batch, 8)?;
    let fetched_at = col::<Int64Array>(batch, 9)?;
    let mut bars = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        bars.push(BarRow {
            date: days_to_date(dates.value(i))?,
            open: open.value(i),
            high: high.value(i),
            low: low.value(i),
            close: close.value(i),
            volume: volume.value(i),
            amount: if amount.is_null(i) {
                None
            } else {
                Some(amount.value(i))
            },
            turnover: if turnover.is_null(i) {
                None
            } else {
                Some(turnover.value(i))
            },
            source: source.value(i).to_string(),
            fetched_at: fetched_at.value(i),
        });
    }
    Ok(bars)
}

fn flows_to_batch(flows: &[FundFlowRow]) -> Result<RecordBatch> {
    let batch = RecordBatch::try_new(
        fund_flow_schema(),
        vec![
            Arc::new(Date32Array::from(
                flows
                    .iter()
                    .map(|f| date_to_days(f.date))
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(Float64Array::from(
                flows.iter().map(|f| f.main_net_inflow).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                flows.iter().map(|f| f.super_large).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                flows.iter().map(|f| f.large).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                flows.iter().map(|f| f.medium).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                flows.iter().map(|f| f.small).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                flows.iter().map(|f| f.source.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                flows.iter().map(|f| f.fetched_at).collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

fn batch_to_flows(batch: &RecordBatch) -> Result<Vec<FundFlowRow>> {
    fn col<T: 'static>(batch: &RecordBatch, idx: usize) -> Result<&T> {
        batch
            .column(idx)
            .as_any()
            .downcast_ref::<T>()
            .ok_or_else(|| Error::Invalid(format!("unexpected parquet column type at {idx}")))
    }
    let dates = col::<Date32Array>(batch, 0)?;
    let main = col::<Float64Array>(batch, 1)?;
    let super_large = col::<Float64Array>(batch, 2)?;
    let large = col::<Float64Array>(batch, 3)?;
    let medium = col::<Float64Array>(batch, 4)?;
    let small = col::<Float64Array>(batch, 5)?;
    let source = col::<StringArray>(batch, 6)?;
    let fetched_at = col::<Int64Array>(batch, 7)?;
    let opt = |arr: &Float64Array, i: usize| {
        if arr.is_null(i) {
            None
        } else {
            Some(arr.value(i))
        }
    };
    let mut flows = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        flows.push(FundFlowRow {
            date: days_to_date(dates.value(i))?,
            main_net_inflow: opt(main, i),
            super_large: opt(super_large, i),
            large: opt(large, i),
            medium: opt(medium, i),
            small: opt(small, i),
            source: source.value(i).to_string(),
            fetched_at: fetched_at.value(i),
        });
    }
    Ok(flows)
}

/// Read every record batch of a parquet file and convert with `convert`.
fn read_batches<T>(
    path: &Path,
    convert: impl Fn(&RecordBatch) -> Result<Vec<T>>,
) -> Result<Vec<T>> {
    let file = File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut out = Vec::new();
    for batch in reader {
        out.extend(convert(&batch?)?);
    }
    Ok(out)
}

/// Write `batch` to `path` atomically (write temp file, then rename).
fn write_batch(path: &Path, batch: &RecordBatch) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("parquet.tmp");
    {
        let file = File::create(&tmp)?;
        let mut writer = ArrowWriter::try_new(file, batch.schema(), None)?;
        writer.write(batch)?;
        writer.close()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Make a string safe for use as a single path component.
fn sanitize_component(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Filesystem layout and IO for the time-series cache.
#[derive(Debug, Clone)]
pub(crate) struct TimeSeriesStore {
    root: PathBuf,
}

impl TimeSeriesStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        TimeSeriesStore { root }
    }

    fn bar_path(&self, symbol: &str, period: &str, adjust: &str) -> PathBuf {
        self.root
            .join(sanitize_component(symbol))
            .join(sanitize_component(period))
            .join(format!("{}.parquet", sanitize_component(adjust)))
    }

    fn fund_flow_path(&self, symbol: &str) -> PathBuf {
        self.root
            .join("fund_flow")
            .join(format!("{}.parquet", sanitize_component(symbol)))
    }

    /// Load all cached bars for `(symbol, period, adjust)`; empty if absent.
    pub(crate) fn load_bars(
        &self,
        symbol: &str,
        period: &str,
        adjust: &str,
    ) -> Result<Vec<BarRow>> {
        let path = self.bar_path(symbol, period, adjust);
        if !path.exists() {
            return Ok(Vec::new());
        }
        read_batches(&path, batch_to_bars)
    }

    /// Overwrite the bar cache for `(symbol, period, adjust)`.
    pub(crate) fn write_bars(
        &self,
        symbol: &str,
        period: &str,
        adjust: &str,
        bars: &[BarRow],
    ) -> Result<()> {
        let batch = bars_to_batch(bars)?;
        write_batch(&self.bar_path(symbol, period, adjust), &batch)
    }

    /// Incrementally merge `new_bars` into the cache: rows are keyed by date,
    /// new rows override existing ones, result is sorted by date.
    pub(crate) fn merge_and_write_bars(
        &self,
        symbol: &str,
        period: &str,
        adjust: &str,
        new_bars: &[BarRow],
    ) -> Result<usize> {
        let mut by_date: BTreeMap<NaiveDate, BarRow> = self
            .load_bars(symbol, period, adjust)?
            .into_iter()
            .map(|b| (b.date, b))
            .collect();
        for bar in new_bars {
            by_date.insert(bar.date, bar.clone());
        }
        let merged: Vec<BarRow> = by_date.into_values().collect();
        let len = merged.len();
        self.write_bars(symbol, period, adjust, &merged)?;
        Ok(len)
    }

    /// Latest cached bar date, for incremental-update decisions.
    pub(crate) fn last_bar_date(
        &self,
        symbol: &str,
        period: &str,
        adjust: &str,
    ) -> Result<Option<NaiveDate>> {
        let path = self.bar_path(symbol, period, adjust);
        if !path.exists() {
            return Ok(None);
        }
        Ok(read_batches(&path, batch_to_bars)?
            .iter()
            .map(|b| b.date)
            .max())
    }

    /// Load the fund-flow series for `symbol`; empty if absent.
    pub(crate) fn load_fund_flow(&self, symbol: &str) -> Result<Vec<FundFlowRow>> {
        let path = self.fund_flow_path(symbol);
        if !path.exists() {
            return Ok(Vec::new());
        }
        read_batches(&path, batch_to_flows)
    }

    /// Merge fund-flow rows keyed by date (same semantics as bars).
    pub(crate) fn merge_and_write_fund_flow(
        &self,
        symbol: &str,
        new_rows: &[FundFlowRow],
    ) -> Result<usize> {
        let mut by_date: BTreeMap<NaiveDate, FundFlowRow> = self
            .load_fund_flow(symbol)?
            .into_iter()
            .map(|f| (f.date, f))
            .collect();
        for row in new_rows {
            by_date.insert(row.date, row.clone());
        }
        let merged: Vec<FundFlowRow> = by_date.into_values().collect();
        let len = merged.len();
        let batch = flows_to_batch(&merged)?;
        write_batch(&self.fund_flow_path(symbol), &batch)?;
        Ok(len)
    }

    /// All parquet files under the cache root (for stats/cleanup).
    pub(crate) fn parquet_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_parquet_files(&self.root, &mut files);
        files
    }
}

fn collect_parquet_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_parquet_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "parquet") {
            out.push(path);
        }
    }
}
