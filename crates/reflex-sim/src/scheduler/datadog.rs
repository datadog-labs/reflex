// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Delayed operational evidence, never authority to reserve resources.
use super::judge::{Evaluation, Evaluator, Evidence};
use crate::datadog::{unix_ms, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

const MAX_AGE_MS: u64 = 60_000;
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
pub(super) fn new_run() -> String {
    format!(
        "scheduler-{}-{}-{}",
        unix_ms(),
        std::process::id(),
        NEXT_RUN.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeEvidence {
    pub node: String,
    pub cpu_capacity: f64,
    pub cpu_reserved: f64,
    pub memory_capacity_bytes: f64,
    pub memory_reserved_bytes: f64,
    pub running_jobs: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueEvidence {
    pub client: String,
    pub queued_jobs: f64,
    pub oldest_age_seconds: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryEvidence {
    pub source: String,
    pub simulation_run: String,
    pub fetched_at_unix_ms: u64,
    /// All values come from the same complete, non-interpolated time bucket.
    pub observed_at_unix_ms: u64,
    pub nodes: Vec<NodeEvidence>,
    pub queues: Vec<QueueEvidence>,
}
impl TelemetryEvidence {
    pub fn validate(&self, run: &str, not_before: u64, now: u64) -> Result<(), String> {
        if self.source != "datadog"
            || self.simulation_run != run
            || self.fetched_at_unix_ms > now
            || now - self.fetched_at_unix_ms > 15_000
            || self.observed_at_unix_ms < not_before
            || self.observed_at_unix_ms > now
            || now - self.observed_at_unix_ms > MAX_AGE_MS
        {
            return Err("Datadog evidence is stale or belongs to an earlier run/resume".into());
        }
        if self.nodes.len() != 4 || self.queues.is_empty() {
            return Err("Incomplete scheduler telemetry".into());
        }
        for (i, n) in self.nodes.iter().enumerate() {
            if n.node != super::telemetry::node(i)
                || n.cpu_capacity <= 0.
                || n.memory_capacity_bytes <= 0.
                || n.cpu_reserved > n.cpu_capacity
                || n.memory_reserved_bytes > n.memory_capacity_bytes
                || [
                    n.cpu_capacity,
                    n.cpu_reserved,
                    n.memory_capacity_bytes,
                    n.memory_reserved_bytes,
                    n.running_jobs,
                ]
                .iter()
                .any(|v| !v.is_finite() || *v < 0. || v.fract() != 0.)
            {
                return Err("Invalid node reservation telemetry".into());
            }
        }
        let mut clients = std::collections::BTreeSet::new();
        for q in &self.queues {
            if !clients.insert(&q.client)
                || !q.queued_jobs.is_finite()
                || q.queued_jobs < 0.
                || q.queued_jobs.fract() != 0.
                || !q.oldest_age_seconds.is_finite()
                || q.oldest_age_seconds < 0.
            {
                return Err("Invalid queue telemetry".into());
            }
        }
        Ok(())
    }
}
impl Source {
    #[tracing::instrument(skip_all, name = "datadog.scheduler.evidence")]
    pub(crate) async fn fetch_scheduler(
        &self,
        run: &str,
        clients: &[u64],
        not_before: u64,
    ) -> Result<TelemetryEvidence, String> {
        let service = std::env::var("DD_SERVICE").unwrap_or_else(|_| "reflex".into());
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
        };
        if !valid(run) || !valid(&service) {
            return Err("Invalid scheduler telemetry scope".into());
        }
        let now = unix_ms();
        let to = now.saturating_sub(20_000) / 10_000 * 10_000;
        if to <= not_before {
            return Err("Warming up: waiting for post-resume Datadog observations".into());
        }
        let scope = format!(
            "env:{},service:{service},policy:jev,simulation_run:{run}",
            self.env
        );
        let queries:Vec<_>=METRICS.iter().enumerate().map(|(i,m)| json!({
            "data_source":"metrics","name":format!("q{i}"),
            "query":format!("max:{m}{{{scope}}} by {{{}}}.rollup(max,10).fill(null)", if i<5 {"node"} else {"client"})
        })).collect();
        let value=self.post("/api/v2/query/timeseries",json!({"data":{"type":"timeseries_request","attributes":{
            "from":to.saturating_sub(60_000).max(not_before),"to":to,"interval":10_000,"queries":queries
        }}})).await?;
        parse(&value, run, clients, not_before, unix_ms())
    }
}
const METRICS: [&str; 7] = [
    "scheduler.node.cpu.capacity",
    "scheduler.node.cpu.reserved",
    "scheduler.node.memory.capacity",
    "scheduler.node.memory.reserved",
    "scheduler.node.jobs.running",
    "scheduler.queue.depth",
    "scheduler.queue.oldest_age",
];
fn parse(
    value: &Value,
    run: &str,
    clients: &[u64],
    not_before: u64,
    now: u64,
) -> Result<TelemetryEvidence, String> {
    #[derive(Deserialize)]
    struct Series {
        query_index: usize,
        group_tags: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Points {
        times: Vec<u64>,
        series: Vec<Series>,
        values: Vec<Vec<Option<f64>>>,
    }
    let p: Points = serde_json::from_value(
        value
            .pointer("/data/attributes")
            .cloned()
            .ok_or("Missing Datadog data")?,
    )
    .map_err(|_| "Malformed scheduler telemetry")?;
    if p.series.len() != p.values.len()
        || p.times
            .windows(2)
            .any(|w| w[1] <= w[0] || w[1] - w[0] != 10_000)
    {
        return Err("Invalid scheduler telemetry buckets".into());
    }
    let mut rows = BTreeMap::new();
    for (s, vs) in p.series.iter().zip(&p.values) {
        if s.query_index >= 7 || vs.len() != p.times.len() {
            return Err("Invalid scheduler telemetry series".into());
        }
        let prefix = if s.query_index < 5 {
            "node:"
        } else {
            "client:"
        };
        let tags: Vec<_> = s
            .group_tags
            .iter()
            .filter_map(|t| t.strip_prefix(prefix))
            .collect();
        if tags.len() != 1
            || rows
                .insert((s.query_index, tags[0].to_owned()), vs)
                .is_some()
        {
            return Err("Duplicate or missing scheduler group".into());
        }
    }
    let keys: Vec<_> = (0..5)
        .flat_map(|q| (0..4).map(move |n| (q, super::telemetry::node(n).to_owned())))
        .chain((5..7).flat_map(|q| {
            clients
                .iter()
                .map(move |id| (q, super::telemetry::client(*id)))
        }))
        .collect();
    let index = (0..p.times.len())
        .rev()
        .find(|i| {
            p.times[*i] >= not_before
                && p.times[*i].saturating_add(10_000)
                    <= now.saturating_sub(20_000) / 10_000 * 10_000
                && keys
                    .iter()
                    .all(|k| rows.get(k).and_then(|r| r[*i]).is_some())
        })
        .ok_or("Warming up: no complete Datadog snapshot for all nodes and clients")?;
    let get = |q: usize, name: &str| rows[&(q, name.to_owned())][index].unwrap();
    let evidence = TelemetryEvidence {
        source: "datadog".into(),
        simulation_run: run.into(),
        fetched_at_unix_ms: now,
        observed_at_unix_ms: p.times[index],
        nodes: (0..4)
            .map(|n| {
                let name = super::telemetry::node(n);
                NodeEvidence {
                    node: name.into(),
                    cpu_capacity: get(0, name),
                    cpu_reserved: get(1, name),
                    memory_capacity_bytes: get(2, name),
                    memory_reserved_bytes: get(3, name),
                    running_jobs: get(4, name),
                }
            })
            .collect(),
        queues: clients
            .iter()
            .map(|id| {
                let name = super::telemetry::client(*id);
                QueueEvidence {
                    client: name.clone(),
                    queued_jobs: get(5, &name),
                    oldest_age_seconds: get(6, &name),
                }
            })
            .collect(),
    };
    evidence.validate(run, not_before, now)?;
    Ok(evidence)
}
pub struct DatadogEvaluator {
    inner: Arc<dyn Evaluator>,
    source: Arc<Source>,
}
impl DatadogEvaluator {
    pub fn new(inner: Arc<dyn Evaluator>, source: Source) -> Self {
        Self {
            inner,
            source: Arc::new(source),
        }
    }
}
impl Evaluator for DatadogEvaluator {
    fn evidence_source(&self) -> Option<Arc<Source>> {
        Some(self.source.clone())
    }
    fn evaluate(&self, evidence: Evidence) -> Evaluation<'_> {
        self.inner.evaluate(evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(at: u64) -> Value {
        let mut series = vec![];
        let mut values = vec![];
        for q in 0..7 {
            let count = if q < 5 { 4 } else { 3 };
            for i in 0..count {
                let tag = if q < 5 {
                    format!("node:{}", super::super::telemetry::node(i))
                } else {
                    format!("client:client_{}", i + 1)
                };
                series.push(json!({"query_index":q,"group_tags":[tag]}));
                values.push(json!([match q {
                    0 => 16.,
                    1 => 2.,
                    2 => 32. * 1073741824.,
                    3 => 4. * 1073741824.,
                    4 => 1.,
                    5 => 3.,
                    _ => 5.,
                }]));
            }
        }
        json!({"data":{"attributes":{"times":[at],"series":series,"values":values}}})
    }
    #[test]
    fn complete_snapshot_rejects_missing_invalid_stale_or_mismatched_evidence() {
        let at = 100_000;
        let good = fixture(at);
        let e = parse(&good, "run", &[0, 1, 2], at, at + 30_000).unwrap();
        assert_eq!(e.nodes.len(), 4);
        assert_eq!(e.queues.len(), 3);
        assert!(e.validate("other", at, at + 30_000).is_err());
        assert!(e.validate("run", at + 1, at + 30_000).is_err());
        assert!(e.validate("run", at, at + 46_000).is_err()); // expired query cache
        assert!(parse(&good, "run", &[0, 1, 2], at, at + 61_000).is_err());
        assert!(parse(&good, "run", &[0, 1, 2, 3], at, at + 30_000).is_err());
        for replacement in [Value::Null, json!(-1), json!(1000)] {
            let mut bad = good.clone();
            bad["data"]["attributes"]["values"][4][0] = replacement;
            assert!(parse(&bad, "run", &[0, 1, 2], at, at + 30_000).is_err());
        }
        let mut reordered = good.clone();
        reordered["data"]["attributes"]["series"]
            .as_array_mut()
            .unwrap()
            .reverse();
        reordered["data"]["attributes"]["values"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert_eq!(
            parse(&reordered, "run", &[0, 1, 2], at, at + 30_000)
                .unwrap()
                .nodes[0]
                .cpu_reserved,
            2.
        );
        let mut duplicate = good.clone();
        duplicate["data"]["attributes"]["series"][1] =
            duplicate["data"]["attributes"]["series"][0].clone();
        assert!(parse(&duplicate, "run", &[0, 1, 2], at, at + 30_000).is_err());
    }
    #[tokio::test]
    async fn query_is_scoped_authenticated_and_does_not_interpolate() {
        use axum::{http::HeaderMap, routing::post, Json, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/api/v2/query/timeseries",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["DD-API-KEY"], "test-api");
                assert_eq!(headers["DD-APPLICATION-KEY"], "test-app");
                let qs = body["data"]["attributes"]["queries"].as_array().unwrap();
                assert_eq!(qs.len(), 7);
                for q in qs {
                    let q = q["query"].as_str().unwrap();
                    assert!(q.contains("simulation_run:wire-run"));
                    assert!(q.contains("policy:jev"));
                    assert!(q.contains("env:local"));
                    assert!(q.contains("service:"));
                    assert!(q.ends_with(".fill(null)"));
                }
                Json(fixture(unix_ms() / 10_000 * 10_000 - 30_000))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let source = Source::for_test(&format!("http://{address}"));
        let e = source
            .fetch_scheduler("wire-run", &[0, 1, 2], unix_ms() - 60_000)
            .await
            .unwrap();
        assert_eq!(e.source, "datadog");
        server.abort();
        assert!(source
            .fetch_scheduler("wire-run", &[0, 1, 2], unix_ms() - 60_000)
            .await
            .is_err());
    }
}
