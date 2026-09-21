//! Forecasting is an application-owned dependency; Reflex receives plain typed evidence.
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
pub const CONTEXT: usize = 256;
pub const HORIZON: usize = 120;
pub const MAX_AGE_MS: u64 = 30_000;
pub const EPOCH: i64 = 1_780_000_000;
pub const SERIES: [&str; 3] = [
    "offered_jobs_per_second",
    "offered_cpu_seconds_per_second",
    "offered_gib_seconds_per_second",
];
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub origin_ms: u64,
    pub timestamps: Vec<i64>,
    pub values: Vec<Vec<f32>>,
    pub prediction_length: usize,
    #[serde(default = "one_second", skip_serializing_if = "is_one_second")]
    pub interval_ms: u64,
}
fn one_second() -> u64 {
    1000
}
fn is_one_second(v: &u64) -> bool {
    *v == 1000
}
impl Input {
    pub fn validate(&self) -> Result<(), String> {
        if !self.origin_ms.is_multiple_of(1000)
            || !((if self.interval_ms == 1000 { 64 } else { 16 })..=8192)
                .contains(&self.values.len())
            || ![1000, 10000].contains(&self.interval_ms)
            || self.timestamps.len() != self.values.len()
            || !(1..=HORIZON).contains(&self.prediction_length)
        {
            return Err("Invalid history length, timestamps or forecast horizon".into());
        }
        let n = self.values.first().map_or(0, Vec::len);
        if n != 3
            || self
                .values
                .iter()
                .any(|v| v.len() != n || v.iter().any(|x| !x.is_finite()))
        {
            return Err("Expected three finite, aligned observation series".into());
        }
        if self
            .timestamps
            .windows(2)
            .any(|w| w[1].checked_sub(w[0]) != Some((self.interval_ms / 1000) as i64))
            || self.timestamps.last().copied() != Some(EPOCH + (self.origin_ms / 1000) as i64)
        {
            return Err(
                "Forecast history must end at its origin on the declared interval grid".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Series {
    pub median: Vec<f32>,
    pub lower: Vec<f32>,
    pub upper: Vec<f32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub origin_ms: u64,
    pub interval_ms: u64,
    pub request_id: String,
    pub source: String,
    pub model_provenance: String,
    pub quantiles: [f32; 3],
    pub series: Vec<Series>,
    pub latency_ms: f64,
}
impl Snapshot {
    pub fn validate(&self, input: &Input) -> Result<(), String> {
        input.validate()?;
        if self.origin_ms != input.origin_ms
            || self.interval_ms != input.interval_ms
            || self.series.len() != 3
            || self.quantiles != [0.1, 0.5, 0.9]
            || !self.latency_ms.is_finite()
            || self.latency_ms < 0.
        {
            return Err(
                "Forecast provenance, dimensions or quantiles do not match the request".into(),
            );
        }
        for s in &self.series {
            if [s.median.len(), s.lower.len(), s.upper.len()] != [input.prediction_length; 3] {
                return Err("Incomplete forecast arrays".into());
            }
            for i in 0..input.prediction_length {
                if !s.median[i].is_finite()
                    || !s.lower[i].is_finite()
                    || !s.upper[i].is_finite()
                    || s.lower[i] > s.median[i]
                    || s.median[i] > s.upper[i]
                {
                    return Err("Forecast has non-finite or crossing quantiles".into());
                }
            }
        }
        Ok(())
    }
    pub fn fresh(&self, now: u64) -> bool {
        self.origin_ms <= now && now - self.origin_ms <= MAX_AGE_MS
    }
    /// Negative forecasts are retained in the export; physical demand summaries clamp at zero.
    pub fn peak(&self, series: usize, upper: bool, now: u64) -> f64 {
        let start = ((now.saturating_sub(self.origin_ms)) / 1000) as usize;
        self.series.get(series).map_or(0., |s| {
            let v = if upper { &s.upper } else { &s.median };
            v.iter().skip(start).copied().fold(0f32, f32::max) as f64
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultRecord {
    pub input: Input,
    pub available_at_ms: u64,
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
}
pub type ForecastFuture<'a> = Pin<Box<dyn Future<Output = Result<Snapshot, String>> + Send + 'a>>;
pub trait Forecaster: Send + Sync {
    fn forecast(&self, input: Input) -> ForecastFuture<'_>;
    fn description(&self) -> String;
}
