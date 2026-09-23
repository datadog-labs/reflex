// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Shared, bounded Toto sidecar for the three interactive scenarios.
//! Forecasts describe continuation of recent observations, not counterfactual actions.
use crate::capacity::forecast::{Forecaster, Input, Snapshot, EPOCH};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub time_domain: String,
    pub origin_ms: u64,
    pub age_ms: u64,
    pub history_seconds: u64,
    pub horizon_seconds: u64,
    pub model_provenance: String,
    pub series: Vec<Prediction>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prediction {
    pub name: String,
    pub buckets: Vec<Bucket>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bucket {
    pub from_seconds: u64,
    pub through_seconds: u64,
    pub p10: f64,
    pub p50: f64,
    pub p90: f64,
}
#[derive(Serialize)]
pub struct ComparisonPoint {
    pub from_ms: u64,
    pub through_ms: u64,
    pub p10: f64,
    pub p50: f64,
    pub p90: f64,
    pub actual: Option<f64>,
}
#[derive(Serialize)]
pub struct ComparisonSeries {
    pub name: String,
    pub points: Vec<ComparisonPoint>,
}
#[derive(Serialize)]
pub struct Comparison {
    pub origin_ms: u64,
    pub available_at_ms: u64,
    pub horizon_ms: u64,
    pub series: Vec<ComparisonSeries>,
}
#[derive(Serialize)]
pub struct View {
    pub minimum_history_seconds: usize,
    pub now_ms: u64,
    pub source: String,
    pub enabled: bool,
    pub configured: bool,
    pub comparisons: Vec<Comparison>,
    pub status: String,
    pub calls: usize,
    pub limit: usize,
    pub samples: usize,
    pub forecast: Option<Evidence>,
}
struct Pending {
    task: JoinHandle<Result<(Input, Snapshot), String>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub struct Driver {
    pub local_min_samples: usize,
    pub enabled: bool,
    actuals: BTreeMap<u64, [f32; 3]>,
    archive: VecDeque<(Input, Snapshot, u64)>,
    pub provider: Option<Arc<dyn Forecaster>>,
    rows: VecDeque<(u64, [f32; 3])>,
    latest: Option<(Input, Snapshot)>,
    pending: Option<Pending>,
    last_wall: Option<Instant>,
    last_origin: u64,
    calls: usize,
    status: String,
    names: [String; 3],
    source: String,
    datadog: bool,
}
impl Default for Driver {
    fn default() -> Self {
        Self::new(None)
    }
}
impl Driver {
    pub fn new(provider: Option<Arc<dyn Forecaster>>) -> Self {
        Self {
            local_min_samples: 64,
            enabled: true,
            actuals: BTreeMap::new(),
            archive: VecDeque::new(),
            provider,
            rows: VecDeque::new(),
            latest: None,
            pending: None,
            last_wall: None,
            last_origin: 0,
            calls: 0,
            status: "Collecting observed history".into(),
            names: Default::default(),
            source: "simulator_observations".into(),
            datadog: false,
        }
    }
    pub fn reset(&mut self) {
        let enabled = self.enabled;
        let local_min_samples = self.local_min_samples;
        *self = Self::new(self.provider.clone());
        self.enabled = enabled;
        self.local_min_samples = local_min_samples;
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.pending = None;
        self.latest = None;
        self.status = if enabled {
            "Collecting observed history"
        } else {
            "Toto off; using observed state"
        }
        .into();
    }
    fn record_actual(&mut self, at: u64, values: [f32; 3]) {
        self.actuals.insert(at, values);
        while self.actuals.len() > 1024 {
            self.actuals.pop_first();
        }
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
    fn now(&self, sim: u64) -> u64 {
        if self.datadog {
            crate::datadog::unix_ms().saturating_sub(EPOCH as u64 * 1000)
        } else {
            sim
        }
    }
    pub fn sample(&mut self, at: u64, values: [f32; 3], names: [&str; 3]) {
        if at == 0
            || self.datadog
            || self.provider.is_none()
            || !at.is_multiple_of(1000)
            || values.iter().any(|v| !v.is_finite() || *v < 0.)
        {
            return;
        }
        if self.rows.back().is_some_and(|(t, _)| *t >= at) {
            return;
        }
        if self.rows.back().is_some_and(|(t, _)| *t + 1000 != at) {
            self.rows.clear();
            self.latest = None;
            self.pending = None;
        }
        self.record_actual(at, values);
        self.names = names.map(str::to_owned);
        self.rows.push_back((at, values));
        while self.rows.len() > 256 {
            self.rows.pop_front();
        }
    }
    pub async fn poll(
        &mut self,
        sim: u64,
        dd: Option<(Arc<crate::datadog::Source>, String, u64, &str)>,
    ) {
        self.datadog = dd.is_some();
        if !self.enabled {
            return;
        }
        let now = self.now(sim);
        if self.pending.as_ref().is_some_and(|p| p.task.is_finished()) {
            let mut pending = self.pending.take().unwrap();
            match (&mut pending.task).await {
                Ok(Ok((input, snapshot))) => {
                    self.status = "Toto forecast ready".into();
                    if self.datadog {
                        for (t, row) in input.timestamps.iter().zip(&input.values) {
                            self.record_actual(
                                (*t - EPOCH) as u64 * 1000,
                                [row[0], row[1], row[2]],
                            );
                        }
                    }
                    self.archive
                        .push_back((input.clone(), snapshot.clone(), now));
                    while self.archive.len() > 12 {
                        self.archive.pop_front();
                    }
                    self.latest = Some((input, snapshot));
                }
                Ok(Err(e)) => self.status = format!("Forecast unavailable: {e}"),
                Err(_) => self.status = "Forecast task stopped".into(),
            }
        }
        let Some(provider) = self.provider.clone() else {
            return;
        };
        if self.pending.is_some()
            || self.calls >= 60
            || self
                .last_wall
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
            || now < self.last_origin + 10_000
        {
            return;
        }
        if dd.is_none() && self.rows.len() < self.local_min_samples {
            self.status = format!(
                "Collecting history: {} / {} seconds",
                self.rows.len(),
                self.local_min_samples
            );
            return;
        }
        if let Some((_, _, floor, _)) = &dd {
            if crate::datadog::unix_ms().saturating_sub(20_000) / 10_000 * 10_000
                < floor.div_ceil(10_000) * 10_000 + 160_000
            {
                self.status =
                    "Collecting at least 160 seconds of Datadog history, plus ingestion delay"
                        .into();
                return;
            }
        }
        let input = Input {
            origin_ms: self.rows.back().map_or(0, |r| r.0),
            timestamps: self
                .rows
                .iter()
                .map(|r| EPOCH + (r.0 / 1000) as i64)
                .collect(),
            values: self.rows.iter().map(|r| r.1.to_vec()).collect(),
            prediction_length: 120,
            interval_ms: 1000,
        };
        if let Some((_, _, _, domain)) = &dd {
            self.names = dd_names(domain).map(str::to_owned);
            self.source = "datadog_observations".into();
        }
        self.last_origin = now;
        self.last_wall = Some(Instant::now());
        self.calls += 1;
        self.status = "Forecast request in flight".into();
        let dd = dd.map(|(s, r, f, d)| (s, r, f, d.to_owned()));
        self.pending = Some(Pending {
            task: tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(12), async {
                    let input = if let Some((source, run, floor, domain)) = dd {
                        source.forecast_history(&run, floor, &domain).await?
                    } else {
                        input
                    };
                    let snapshot = provider.forecast(input.clone()).await?;
                    snapshot.validate(&input)?;
                    Ok((input, snapshot))
                })
                .await
                .map_err(|_| "Forecast deadline exceeded".to_owned())?
            }),
        });
    }
    pub fn evidence(&self, sim: u64) -> Option<Evidence> {
        if !self.enabled {
            return None;
        }
        let (input, snapshot) = self.latest.as_ref()?;
        let now = self.now(sim);
        if snapshot.origin_ms > now
            || now - snapshot.origin_ms > if self.datadog { 60_000 } else { 30_000 }
        {
            return None;
        }
        let step = (10_000 / input.interval_ms).max(1) as usize;
        let start = ((now - snapshot.origin_ms) / input.interval_ms) as usize;
        let series = snapshot
            .series
            .iter()
            .enumerate()
            .map(|(k, s)| {
                let buckets = (start..s.median.len())
                    .step_by(step)
                    .map(|i| {
                        let end = (i + step).min(s.median.len());
                        let mean = |v: &Vec<f32>| {
                            v[i..end].iter().map(|x| x.max(0.) as f64).sum::<f64>()
                                / (end - i) as f64
                        };
                        Bucket {
                            from_seconds: (i as u64 + 1) * input.interval_ms / 1000,
                            through_seconds: end as u64 * input.interval_ms / 1000,
                            p10: mean(&s.lower),
                            p50: mean(&s.median),
                            p90: mean(&s.upper),
                        }
                    })
                    .collect();
                Prediction {
                    name: self.names[k].clone(),
                    buckets,
                }
            })
            .collect();
        Some(Evidence {
            source: self.source.clone(),
            time_domain: if self.datadog {
                "forecast_epoch_wall_clock"
            } else {
                "simulation"
            }
            .into(),
            origin_ms: snapshot.origin_ms,
            age_ms: now - snapshot.origin_ms,
            history_seconds: input.values.len() as u64 * input.interval_ms / 1000,
            horizon_seconds: input.prediction_length as u64 * input.interval_ms / 1000,
            model_provenance: snapshot.model_provenance.clone(),
            series,
        })
    }
    fn comparisons(&self) -> Vec<Comparison> {
        if !self.enabled {
            return vec![];
        }
        self.archive
            .iter()
            .map(|(input, snapshot, available)| {
                let step = (10_000 / input.interval_ms).max(1) as usize;
                Comparison {
                    origin_ms: input.origin_ms,
                    available_at_ms: *available,
                    horizon_ms: input.prediction_length as u64 * input.interval_ms,
                    series: snapshot
                        .series
                        .iter()
                        .enumerate()
                        .map(|(k, s)| ComparisonSeries {
                            name: self.names[k].clone(),
                            points: (0..input.prediction_length)
                                .step_by(step)
                                .map(|i| {
                                    let end = (i + step).min(input.prediction_length);
                                    let mean = |v: &Vec<f32>| {
                                        v[i..end].iter().map(|v| v.max(0.) as f64).sum::<f64>()
                                            / (end - i) as f64
                                    };
                                    let actual = (i..end)
                                        .map(|j| {
                                            self.actuals
                                                .get(
                                                    &(input.origin_ms
                                                        + (j as u64 + 1) * input.interval_ms),
                                                )
                                                .map(|v| v[k] as f64)
                                        })
                                        .collect::<Option<Vec<_>>>()
                                        .map(|v| v.iter().sum::<f64>() / v.len() as f64);
                                    ComparisonPoint {
                                        from_ms: input.origin_ms
                                            + (i as u64 + 1) * input.interval_ms,
                                        through_ms: input.origin_ms
                                            + end as u64 * input.interval_ms,
                                        p10: mean(&s.lower),
                                        p50: mean(&s.median),
                                        p90: mean(&s.upper),
                                        actual,
                                    }
                                })
                                .collect(),
                        })
                        .collect(),
                }
            })
            .collect()
    }
    pub fn view(&self, sim: u64) -> View {
        let forecast = self.evidence(sim);
        let status = if !self.enabled {
            "Toto off; using observed state".into()
        } else if self.provider.is_none() {
            "Toto not configured; using observed state".into()
        } else if self.calls >= 60 {
            "Forecast request limit reached".into()
        } else if self.latest.is_some() && forecast.is_none() && self.pending.is_none() {
            format!("Forecast stale; using observed state. {}", self.status)
        } else {
            self.status.clone()
        };
        View {
            minimum_history_seconds: if self.datadog {
                160
            } else {
                self.local_min_samples
            },
            now_ms: self.now(sim),
            source: self.source.clone(),
            enabled: self.enabled,
            configured: self.provider.is_some(),
            comparisons: self.comparisons(),
            status,
            calls: self.calls,
            limit: 60,
            samples: if self.datadog {
                self.latest
                    .as_ref()
                    .map_or(0, |(input, _)| input.values.len())
            } else {
                self.rows.len()
            },
            forecast,
        }
    }
}
fn dd_names(domain: &str) -> [&'static str; 3] {
    match domain {
        "scheduler" => ["queue_depth", "reserved_cpu", "reserved_memory_gib"],
        "recovery" => [
            "replica_queue_depth",
            "active_requests",
            "outstanding_requests",
        ],
        _ => ["queue_depth", "active_requests", "utilization"],
    }
}
impl crate::datadog::Source {
    async fn forecast_history(&self, run: &str, floor: u64, domain: &str) -> Result<Input, String> {
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
        };
        let service = std::env::var("DD_SERVICE").unwrap_or_else(|_| "reflex".into());
        if !valid(run) || !valid(domain) || !valid(&service) {
            return Err("Invalid forecast scope".into());
        }
        let now = crate::datadog::unix_ms();
        let to = now.saturating_sub(20_000) / 10_000 * 10_000;
        let from = to
            .saturating_sub(320_000)
            .max(floor.div_ceil(10_000) * 10_000);
        if to < from + 160_000 {
            return Err("Collecting at least 160 seconds of Datadog history".into());
        }
        let base = format!("env:{},policy:jev,simulation_run:{run}", self.env);
        let definitions = match domain {
            "scheduler" => vec![
                ("scheduler.queue.depth", service.clone()),
                ("scheduler.node.cpu.reserved", service.clone()),
                ("scheduler.node.memory.reserved", service),
            ],
            "recovery" => vec![
                ("http.server.queue.depth", "replica_pool".into()),
                ("http.server.active", "replica_pool".into()),
                ("recovery.replica.requests.outstanding", service),
            ],
            _ => vec![
                ("http.server.queue.depth", domain.into()),
                ("http.server.active", domain.into()),
                ("http.server.utilization", domain.into()),
            ],
        };
        let queries:Vec<_>=definitions.into_iter().enumerate().map(|(i,(metric,service))|serde_json::json!({"data_source":"metrics","name":format!("q{i}"),"query":format!("sum:{metric}{{{base},service:{service}}}.rollup(avg,10).fill(null)")})).collect();
        let body = serde_json::json!({"data":{"type":"timeseries_request","attributes":{"from":from,"to":to,"interval":10000,"queries":queries}}});
        let response = self.post("/api/v2/query/timeseries", body).await?;
        parse_datadog(&response, from, to)
    }
}
fn parse_datadog(v: &serde_json::Value, from: u64, to: u64) -> Result<Input, String> {
    let a = &v["data"]["attributes"];
    let times = a["times"].as_array().ok_or("Missing forecast timestamps")?;
    let series = a["series"].as_array().ok_or("Missing forecast series")?;
    let values = a["values"].as_array().ok_or("Missing forecast values")?;
    if values.len() != series.len()
        || values
            .iter()
            .any(|v| v.as_array().is_none_or(|v| v.len() != times.len()))
    {
        return Err("Misaligned Datadog forecast dimensions".into());
    }
    let mut columns = [None; 3];
    for (i, s) in series.iter().enumerate() {
        let k = s["query_index"].as_u64().ok_or("Missing query index")? as usize;
        if k >= 3 || columns[k].replace(i).is_some() {
            return Err("Ambiguous forecast series".into());
        }
    }
    if columns.iter().any(Option::is_none) {
        return Err("Incomplete Datadog forecast series".into());
    }
    let mut rows = Vec::new();
    let mut stamps = Vec::new();
    for (i, t) in times.iter().enumerate() {
        let t = t.as_u64().ok_or("Invalid forecast timestamp")?;
        if !t.is_multiple_of(10_000) {
            return Err("Forecast timestamp is off the ten-second grid".into());
        }
        if t < from || t >= to {
            continue;
        }
        let mut row = Vec::new();
        for k in columns {
            let x = values
                .get(k.unwrap())
                .and_then(|v| v.get(i))
                .and_then(|v| v.as_f64())
                .filter(|x| x.is_finite() && *x >= 0.)
                .ok_or("Missing Datadog forecast bucket; no values were invented")?;
            row.push(x as f32);
        }
        rows.push(row);
        stamps.push((t / 1000) as i64);
    }
    let end = stamps.last().copied().ok_or("Empty forecast history")?;
    let input = Input {
        origin_ms: (end - EPOCH).max(0) as u64 * 1000,
        timestamps: stamps,
        values: rows,
        prediction_length: 12,
        interval_ms: 10000,
    };
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::capacity::forecast::{ForecastFuture, Series};
    use std::sync::Mutex;

    pub(crate) struct Recording(pub Mutex<Vec<Input>>);
    impl Forecaster for Recording {
        fn description(&self) -> String {
            "Test forecaster".into()
        }
        fn forecast(&self, input: Input) -> ForecastFuture<'_> {
            self.0.lock().unwrap().push(input.clone());
            Box::pin(async move { Ok(snapshot(&input)) })
        }
    }
    fn snapshot(input: &Input) -> Snapshot {
        Snapshot {
            origin_ms: input.origin_ms,
            interval_ms: input.interval_ms,
            request_id: "fixture".into(),
            source: "test".into(),
            model_provenance: "test only".into(),
            quantiles: [0.1, 0.5, 0.9],
            latency_ms: 1.,
            series: (0..3)
                .map(|_| Series {
                    lower: vec![-1.; input.prediction_length],
                    median: vec![2.; input.prediction_length],
                    upper: vec![4.; input.prediction_length],
                })
                .collect(),
        }
    }
    async fn complete(driver: &mut Driver, at: u64) {
        for _ in 0..50 {
            tokio::task::yield_now().await;
            driver.poll(at, None).await;
            if driver.pending.is_none() {
                return;
            }
        }
        panic!("forecast task did not complete");
    }
    #[tokio::test]
    async fn observed_history_warmup_freshness_and_reset() {
        let mock = Arc::new(Recording(Mutex::new(vec![])));
        let mut driver = Driver::new(Some(mock.clone()));
        let names = ["arrivals", "failures", "queue"];
        for at in 1..=63 {
            driver.sample(at * 1000, [at as f32, 0., 3.], names);
        }
        driver.poll(63_000, None).await;
        assert_eq!(driver.calls, 0);
        driver.sample(64_000, [64., 0., 3.], names);
        driver.poll(64_000, None).await;
        complete(&mut driver, 64_000).await;
        let inputs = mock.0.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].values.len(), 64);
        assert_eq!(inputs[0].values[0], vec![1., 0., 3.]);
        assert_eq!(inputs[0].timestamps.last(), Some(&(EPOCH + 64)));
        drop(inputs);
        let e = driver.evidence(70_000).unwrap();
        assert_eq!(e.source, "simulator_observations");
        assert_eq!(e.age_ms, 6000);
        assert_eq!(e.series[0].buckets[0].from_seconds, 7);
        assert_eq!(e.series[0].buckets[0].p10, 0.);
        assert!(driver.evidence(63_000).is_none());
        assert!(driver.evidence(94_001).is_none());
        driver.reset();
        assert!(driver.evidence(70_000).is_none());
        assert_eq!(driver.calls, 0);
        assert!(driver.provider.is_some());
    }
    #[tokio::test]
    async fn reset_aborts_inflight_forecast_and_gaps_do_not_get_padded() {
        struct Never;
        impl Forecaster for Never {
            fn description(&self) -> String {
                "never".into()
            }
            fn forecast(&self, _: Input) -> ForecastFuture<'_> {
                Box::pin(std::future::pending())
            }
        }
        let mut driver = Driver::new(Some(Arc::new(Never)));
        for t in 1..=64 {
            driver.sample(t * 1000, [1.; 3], ["a", "b", "c"]);
        }
        driver.poll(64_000, None).await;
        let task = driver.pending.as_ref().unwrap().task.abort_handle();
        driver.reset();
        tokio::task::yield_now().await;
        assert!(task.is_finished());
        driver.sample(1_000, [1.; 3], ["a", "b", "c"]);
        driver.sample(3_000, [1.; 3], ["a", "b", "c"]);
        assert_eq!(driver.rows.len(), 1);
        assert_eq!(driver.rows[0].0, 3000);
    }
    fn dd_fixture() -> (serde_json::Value, u64, u64) {
        let from = (EPOCH as u64 + 1000) * 1000;
        (
            serde_json::json!({"data":{"attributes":{
                "times":(0..16).map(|i|from+i*10000).collect::<Vec<_>>(),
                "series":[{"query_index":2},{"query_index":0},{"query_index":1}],
                "values":[vec![3.;16],vec![1.;16],vec![2.;16]]
            }}}),
            from,
            from + 160000,
        )
    }
    #[test]
    fn datadog_uses_real_aligned_buckets_without_filling_missing_data() {
        let (v, from, to) = dd_fixture();
        let input = parse_datadog(&v, from, to).unwrap();
        assert_eq!(input.interval_ms, 10000);
        assert_eq!(input.values[0], vec![1., 2., 3.]);
        assert_eq!(input.prediction_length, 12);
        assert_eq!(input.origin_ms, 1_150_000);
        let mut missing = v.clone();
        missing["data"]["attributes"]["values"][0][5] = serde_json::Value::Null;
        assert!(parse_datadog(&missing, from, to).is_err());
        let mut gap = v.clone();
        gap["data"]["attributes"]["times"][5] = serde_json::json!(from + 51_000);
        assert!(parse_datadog(&gap, from, to).is_err());
        let mut duplicate = v;
        duplicate["data"]["attributes"]["series"][0]["query_index"] = serde_json::json!(0);
        assert!(parse_datadog(&duplicate, from, to).is_err());
    }
    #[tokio::test]
    async fn datadog_warmup_does_not_spend_budget_or_use_local_samples() {
        let mock = Arc::new(Recording(Mutex::new(vec![])));
        let mut driver = Driver::new(Some(mock.clone()));
        let source = Arc::new(crate::datadog::Source::for_test("http://127.0.0.1:1"));
        driver
            .poll(
                0,
                Some((source, "run".into(), crate::datadog::unix_ms(), "scheduler")),
            )
            .await;
        driver.sample(1000, [1.; 3], ["a", "b", "c"]);
        assert!(driver.rows.is_empty());
        assert_eq!(driver.calls, 0);
        assert!(mock.0.lock().unwrap().is_empty());
        assert!(driver.evidence(1000).is_none());
    }
    #[test]
    fn malformed_forecasts_are_rejected() {
        let (v, from, to) = dd_fixture();
        let input = parse_datadog(&v, from, to).unwrap();
        let mut s = snapshot(&input);
        s.series[0].upper[0] = 1.;
        assert!(s.validate(&input).is_err());
        let mut s = snapshot(&input);
        s.series[0].median.pop();
        assert!(s.validate(&input).is_err());
        let mut s = snapshot(&input);
        s.interval_ms = 1000;
        assert!(s.validate(&input).is_err());
    }
    struct Capture(Arc<Mutex<Vec<serde_json::Value>>>);
    impl crate::scheduler::judge::Evaluator for Capture {
        fn evaluate(
            &self,
            e: crate::scheduler::judge::Evidence,
        ) -> crate::scheduler::judge::Evaluation<'_> {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_value(e).unwrap());
            Box::pin(async { crate::scheduler::judge::Inference::failed("test abstention") })
        }
    }
    impl crate::recovery::judge::Evaluator for Capture {
        fn evaluate(
            &self,
            e: crate::recovery::judge::Evidence,
        ) -> crate::recovery::judge::Evaluation<'_> {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_value(e).unwrap());
            Box::pin(async { crate::recovery::judge::Inference::failed("test abstention") })
        }
    }
    #[tokio::test]
    async fn scheduler_and_recovery_deliver_forecast_to_judge_and_clear_on_reset() {
        let inputs = Arc::new(Mutex::new(vec![]));
        let settings = crate::playground::inference::JevSettings {
            dispatch_interval: Duration::ZERO,
            ..Default::default()
        };
        let mut scheduler = crate::scheduler::Session::new(
            42,
            Some(Arc::new(Capture(inputs.clone()))),
            settings.clone(),
        )
        .unwrap();
        let mock = Arc::new(Recording(Mutex::new(vec![])));
        scheduler.forecast.provider = Some(mock.clone());
        for _ in 0..75 {
            scheduler
                .command(crate::scheduler::Command::Step)
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        assert!(inputs
            .lock()
            .unwrap()
            .iter()
            .any(|i| i["forecast"]["source"] == "simulator_observations"));
        assert!(mock.0.lock().unwrap().iter().all(|i| i.values.len() >= 64));
        assert!(scheduler.view().forecast.forecast.is_some());
        scheduler
            .command(crate::scheduler::Command::Reset)
            .await
            .unwrap();
        assert!(scheduler.view().forecast.forecast.is_none());
        inputs.lock().unwrap().clear();
        let mut recovery =
            crate::recovery::Session::new(42, Some(Arc::new(Capture(inputs.clone()))), settings)
                .unwrap();
        recovery.forecast.provider = Some(mock);
        for _ in 0..75 {
            recovery
                .command(crate::recovery::Command::Step)
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        assert!(inputs
            .lock()
            .unwrap()
            .iter()
            .any(|i| i["forecast"]["source"] == "simulator_observations"));
        assert!(recovery.view().forecast.forecast.is_some());
        recovery
            .command(crate::recovery::Command::Reset)
            .await
            .unwrap();
        assert!(recovery.view().forecast.forecast.is_none());
    }
    #[tokio::test]
    async fn toggle_stops_calls_and_evidence_without_resetting_observations_or_budget() {
        let mock = Arc::new(Recording(Mutex::new(vec![])));
        let mut driver = Driver::new(Some(mock.clone()));
        for t in 1..=64 {
            driver.sample(t * 1000, [1.; 3], ["a", "b", "c"]);
        }
        driver.poll(64_000, None).await;
        complete(&mut driver, 64_000).await;
        assert!(driver.evidence(64_000).is_some());
        driver.set_enabled(false);
        driver.last_wall = None;
        for t in 65..=80 {
            driver.sample(t * 1000, [3.; 3], ["a", "b", "c"]);
            driver.poll(t * 1000, None).await;
        }
        assert_eq!(driver.calls, 1);
        assert!(driver.evidence(80_000).is_none());
        assert!(driver.view(80_000).comparisons.is_empty());
        assert_eq!(driver.rows.len(), 80);
        driver.set_enabled(true);
        assert!(driver.evidence(80_000).is_none());
        driver.poll(80_000, None).await;
        complete(&mut driver, 80_000).await;
        assert_eq!(driver.calls, 2);
        assert_eq!(mock.0.lock().unwrap()[1].values.len(), 80);
        driver.set_enabled(false);
        driver.reset();
        assert!(!driver.enabled);
        assert!(driver.archive.is_empty());
    }
    #[tokio::test]
    async fn comparisons_match_complete_future_buckets_and_keep_original_forecasts() {
        let mut driver = Driver::new(Some(Arc::new(Recording(Mutex::new(vec![])))));
        for t in 1..=64 {
            driver.sample(t * 1000, [100.; 3], ["a", "b", "c"]);
        }
        driver.poll(64_000, None).await;
        complete(&mut driver, 64_000).await;
        assert!(driver.comparisons()[0].series[0]
            .points
            .iter()
            .all(|p| p.actual.is_none()));
        for t in 65..=73 {
            driver.sample(t * 1000, [5.; 3], ["a", "b", "c"]);
        }
        assert!(driver.comparisons()[0].series[0].points[0].actual.is_none());
        driver.sample(74_000, [15.; 3], ["a", "b", "c"]);
        driver.last_wall = None;
        driver.poll(74_000, None).await;
        complete(&mut driver, 74_000).await;
        let comparisons = driver.comparisons();
        assert_eq!(comparisons.len(), 2);
        let point = &comparisons[0].series[0].points[0];
        assert_eq!((point.from_ms, point.through_ms), (65_000, 74_000));
        assert_eq!(point.actual, Some(6.));
        assert_eq!(point.p50, 2.);
        assert!(comparisons[0].series[0].points[1].actual.is_none());
        assert!(comparisons[1].series[0].points[0].actual.is_none());
        // Datadog comparisons use only matched remote observations, not the local sample map.
        let (v, from, to) = dd_fixture();
        let input = parse_datadog(&v, from, to).unwrap();
        let mut remote = Driver {
            datadog: true,
            ..Default::default()
        };
        remote
            .archive
            .push_back((input.clone(), snapshot(&input), input.origin_ms + 20_000));
        remote.record_actual(input.origin_ms + 10_000, [7., 8., 9.]);
        assert_eq!(remote.comparisons()[0].series[0].points[0].actual, Some(7.));
        assert!(remote.comparisons()[0].series[0].points[1].actual.is_none());
    }
}
