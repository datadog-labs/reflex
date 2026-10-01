// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Delayed operational evidence, never authority to change the cluster.
use super::{
    engine::{Group, WORKLOADS},
    judge::{Evaluation, Evaluator, Evidence},
    telemetry::STATES,
};
use crate::datadog::{unix_ms, Source, MAX_AGE_MS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

/// A query result is reused for at most this long before it must be refreshed.
pub const QUERY_TTL_MS: u64 = 15_000;
const BUCKET_MS: u64 = 10_000;
const INGESTION_MARGIN_MS: u64 = 20_000;
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
pub(super) fn new_run() -> String {
    format!(
        "autoscaler-{}-{}-{}",
        unix_ms(),
        std::process::id(),
        NEXT_RUN.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkloadTelemetry {
    pub workload: String,
    pub desired_replicas: f64,
    pub available_replicas: f64,
    pub pending_pods: f64,
    pub oldest_pending_age_seconds: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupTelemetry {
    pub group: String,
    pub ready_nodes: f64,
    pub provisioning_nodes: f64,
    pub draining_nodes: f64,
    pub cpu_capacity: f64,
    pub cpu_reserved: f64,
    pub memory_capacity_bytes: f64,
    pub memory_reserved_bytes: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryEvidence {
    pub source: String,
    pub simulation_run: String,
    pub fetched_at_unix_ms: u64,
    /// All values come from the same complete, non-interpolated ten-second bucket.
    pub observed_at_unix_ms: u64,
    /// Age of that bucket when this evidence was attached to an evaluation.
    pub age_seconds: f64,
    pub requested_cpu: f64,
    pub requested_memory_bytes: f64,
    pub workloads: Vec<WorkloadTelemetry>,
    pub groups: Vec<GroupTelemetry>,
}
impl TelemetryEvidence {
    pub fn validate(&self, run: &str, not_before: u64, now: u64) -> Result<(), String> {
        if self.source != "datadog"
            || self.simulation_run != run
            || self.fetched_at_unix_ms > now
            || now - self.fetched_at_unix_ms > QUERY_TTL_MS
            || self.observed_at_unix_ms < not_before
            || self.observed_at_unix_ms > now
            || now - self.observed_at_unix_ms > MAX_AGE_MS
        {
            return Err("Datadog evidence is stale or belongs to an earlier run/resume".into());
        }
        let whole = |v: f64| v.is_finite() && v >= 0. && v.fract() == 0.;
        if !whole(self.requested_cpu) || !whole(self.requested_memory_bytes) {
            return Err("Invalid cluster demand telemetry".into());
        }
        if self.workloads.len() != WORKLOADS.len()
            || self.workloads.iter().zip(WORKLOADS).any(|(w, name)| {
                w.workload != name
                    || !whole(w.desired_replicas)
                    || !whole(w.available_replicas)
                    || !whole(w.pending_pods)
                    || !w.oldest_pending_age_seconds.is_finite()
                    || w.oldest_pending_age_seconds < 0.
            })
        {
            return Err("Incomplete or invalid workload telemetry".into());
        }
        if self.groups.len() != Group::ALL.len()
            || self.groups.iter().zip(Group::ALL).any(|(g, group)| {
                g.group != group.key()
                    || ![
                        g.ready_nodes,
                        g.provisioning_nodes,
                        g.draining_nodes,
                        g.cpu_capacity,
                        g.cpu_reserved,
                        g.memory_capacity_bytes,
                        g.memory_reserved_bytes,
                    ]
                    .into_iter()
                    .all(whole)
                    || g.cpu_reserved > g.cpu_capacity
                    || g.memory_reserved_bytes > g.memory_capacity_bytes
            })
        {
            return Err("Incomplete or invalid node group telemetry".into());
        }
        Ok(())
    }
    /// The same observations, stamped with their age at the moment they are used.
    pub fn at(mut self, now: u64) -> Self {
        self.age_seconds = now.saturating_sub(self.observed_at_unix_ms) as f64 / 1000.;
        self
    }
}
/// Queried metrics and the tags each is grouped by.
const QUERIES: [(&str, &[&str]); 11] = [
    ("autoscaler.cpu.requested", &[]),
    ("autoscaler.memory.requested", &[]),
    ("autoscaler.pods.pending", &["workload"]),
    ("autoscaler.pods.pending.oldest_age", &["workload"]),
    ("autoscaler.workload.replicas.desired", &["workload"]),
    ("autoscaler.workload.replicas.available", &["workload"]),
    ("autoscaler.nodes", &["group", "state"]),
    ("autoscaler.group.cpu.capacity", &["group"]),
    ("autoscaler.group.cpu.reserved", &["group"]),
    ("autoscaler.group.memory.capacity", &["group"]),
    ("autoscaler.group.memory.reserved", &["group"]),
];
impl Source {
    #[tracing::instrument(skip_all, name = "datadog.autoscaler.evidence")]
    pub(crate) async fn fetch_autoscaler(
        &self,
        run: &str,
        not_before: u64,
    ) -> Result<TelemetryEvidence, String> {
        let service = std::env::var("DD_SERVICE").unwrap_or_else(|_| "reflex".into());
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
        };
        if !valid(run) || !valid(&service) {
            return Err("Invalid autoscaler telemetry scope".into());
        }
        let now = unix_ms();
        let to = now.saturating_sub(INGESTION_MARGIN_MS) / BUCKET_MS * BUCKET_MS;
        if to <= not_before {
            return Err("Warming up: waiting for post-resume Datadog observations".into());
        }
        let scope = format!(
            "env:{},service:{service},policy:jev,simulation_run:{run}",
            self.env
        );
        let queries: Vec<_> = QUERIES
            .iter()
            .enumerate()
            .map(|(i, (metric, by))| {
                let by = if by.is_empty() {
                    String::new()
                } else {
                    format!(" by {{{}}}", by.join(","))
                };
                json!({
                    "data_source":"metrics","name":format!("q{i}"),
                    "query":format!("max:{metric}{{{scope}}}{by}.rollup(max,10).fill(null)")
                })
            })
            .collect();
        let value = self
            .post(
                "/api/v2/query/timeseries",
                json!({"data":{"type":"timeseries_request","attributes":{
                    "from":to.saturating_sub(60_000).max(not_before),"to":to,"interval":BUCKET_MS,"queries":queries
                }}}),
            )
            .await?;
        parse(&value, run, not_before, unix_ms())
    }
}
fn parse(value: &Value, run: &str, not_before: u64, now: u64) -> Result<TelemetryEvidence, String> {
    #[derive(Deserialize)]
    struct Series {
        query_index: usize,
        #[serde(default)]
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
    .map_err(|_| "Malformed autoscaler telemetry")?;
    if p.series.len() != p.values.len()
        || p.times
            .windows(2)
            .any(|w| w[1] <= w[0] || w[1] - w[0] != BUCKET_MS)
    {
        return Err("Invalid autoscaler telemetry buckets".into());
    }
    // Series are identified by query and by the values of the tags that query groups by.
    let mut rows = BTreeMap::new();
    for (s, values) in p.series.iter().zip(&p.values) {
        let (_, by) = QUERIES
            .get(s.query_index)
            .filter(|_| values.len() == p.times.len())
            .ok_or("Invalid autoscaler telemetry series")?;
        let key: Option<Vec<&str>> = by
            .iter()
            .map(|tag| {
                let prefix = format!("{tag}:");
                s.group_tags.iter().find_map(|t| t.strip_prefix(&prefix))
            })
            .collect();
        let key = key.ok_or("Missing autoscaler telemetry group")?.join("/");
        if rows.insert((s.query_index, key), values).is_some() {
            return Err("Duplicate autoscaler telemetry group".into());
        }
    }
    let states = || STATES.iter().map(|(_, name)| *name);
    let keys: Vec<(usize, String)> = (0..2)
        .map(|q| (q, String::new()))
        .chain((2..6).flat_map(|q| WORKLOADS.iter().map(move |w| (q, w.to_string()))))
        .chain(
            Group::ALL
                .iter()
                .flat_map(|g| states().map(move |state| (6, format!("{}/{state}", g.key())))),
        )
        .chain((7..11).flat_map(|q| Group::ALL.iter().map(move |g| (q, g.key().to_owned()))))
        .collect();
    let complete_before = now.saturating_sub(INGESTION_MARGIN_MS) / BUCKET_MS * BUCKET_MS;
    let index = (0..p.times.len())
        .rev()
        .find(|i| {
            p.times[*i] >= not_before
                && p.times[*i].saturating_add(BUCKET_MS) <= complete_before
                && keys
                    .iter()
                    .all(|k| rows.get(k).and_then(|r| r[*i]).is_some())
        })
        .ok_or("Warming up: no complete Datadog snapshot of the cluster yet")?;
    let get = |q: usize, key: &str| rows[&(q, key.to_owned())][index].unwrap();
    let evidence = TelemetryEvidence {
        source: "datadog".into(),
        simulation_run: run.into(),
        fetched_at_unix_ms: now,
        observed_at_unix_ms: p.times[index],
        age_seconds: now.saturating_sub(p.times[index]) as f64 / 1000.,
        requested_cpu: get(0, ""),
        requested_memory_bytes: get(1, ""),
        workloads: WORKLOADS
            .iter()
            .map(|w| WorkloadTelemetry {
                workload: w.to_string(),
                pending_pods: get(2, w),
                oldest_pending_age_seconds: get(3, w),
                desired_replicas: get(4, w),
                available_replicas: get(5, w),
            })
            .collect(),
        groups: Group::ALL
            .iter()
            .map(|g| {
                let nodes = |state: &str| get(6, &format!("{}/{state}", g.key()));
                GroupTelemetry {
                    group: g.key().into(),
                    ready_nodes: nodes("ready"),
                    provisioning_nodes: nodes("provisioning"),
                    draining_nodes: nodes("draining"),
                    cpu_capacity: get(7, g.key()),
                    cpu_reserved: get(8, g.key()),
                    memory_capacity_bytes: get(9, g.key()),
                    memory_reserved_bytes: get(10, g.key()),
                }
            })
            .collect(),
    };
    evidence.validate(run, not_before, now)?;
    Ok(evidence)
}
/// Selects Datadog evidence for the autoscaler without changing the judge.
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
pub(crate) mod tests {
    use super::*;
    /// A complete response: 18 CPU and 40 GiB requested, two pending web pods, one bucket.
    pub(crate) fn fixture(at: u64) -> Value {
        let mut series = vec![];
        let mut values = vec![];
        let mut push = |q: usize, tags: Vec<String>, value: f64| {
            series.push(json!({"query_index":q,"group_tags":tags}));
            values.push(json!([value]));
        };
        push(0, vec![], 18.);
        push(1, vec![], 40. * 1073741824.);
        for (i, w) in WORKLOADS.iter().enumerate() {
            let tag = vec![format!("workload:{w}")];
            push(2, tag.clone(), if i == 0 { 2. } else { 0. });
            push(3, tag.clone(), if i == 0 { 12.5 } else { 0. });
            push(4, tag.clone(), [6., 2., 1.][i]);
            push(5, tag, [4., 2., 1.][i]);
        }
        for (i, g) in Group::ALL.iter().enumerate() {
            let group = format!("group:{}", g.key());
            for (_, state) in STATES {
                // Datadog returns group tags in no particular order.
                let ready = if i < 2 { 2. } else { 0. };
                push(
                    6,
                    vec![format!("state:{state}"), group.clone()],
                    if state == "ready" { ready } else { 0. },
                );
            }
            push(7, vec![group.clone()], [8., 16., 0.][i]);
            push(8, vec![group.clone()], [4., 14., 0.][i]);
            push(9, vec![group.clone()], [16., 32., 0.][i] * 1073741824.);
            push(10, vec![group], [10., 30., 0.][i] * 1073741824.);
        }
        json!({"data":{"attributes":{"times":[at],"series":series,"values":values}}})
    }
    #[test]
    fn complete_snapshot_rejects_missing_invalid_stale_or_mismatched_evidence() {
        let at = 100_000;
        let good = fixture(at);
        let e = parse(&good, "run", at, at + 30_000).unwrap();
        assert_eq!(e.source, "datadog");
        assert_eq!((e.observed_at_unix_ms, e.age_seconds), (at, 30.));
        assert_eq!(e.requested_cpu, 18.);
        assert_eq!(e.workloads[0].workload, "web");
        assert_eq!(e.workloads[0].pending_pods, 2.);
        assert_eq!(e.workloads[0].oldest_pending_age_seconds, 12.5);
        assert_eq!(e.groups[1].group, "general_large");
        assert_eq!(
            (e.groups[1].ready_nodes, e.groups[1].cpu_reserved),
            (2., 14.)
        );
        assert_eq!(e.groups[2].ready_nodes, 0.);
        assert_eq!(e.clone().at(at + 41_000).age_seconds, 41.);

        // Wrong run, an earlier resume boundary, an expired query result, an old observation.
        assert!(e.validate("other", at, at + 30_000).is_err());
        assert!(e.validate("run", at + 1, at + 30_000).is_err());
        assert!(e
            .validate("run", at, at + 30_000 + QUERY_TTL_MS + 1)
            .is_err());
        assert!(parse(&good, "run", at, at + MAX_AGE_MS + 1).is_err());
        // The newest twenty seconds are never used: the bucket may still be filling.
        assert!(parse(&good, "run", at, at + 29_999).is_err());

        let cell = |bad: &mut Value, series: usize, value: Value| {
            bad["data"]["attributes"]["values"][series][0] = value;
        };
        for replacement in [Value::Null, json!(-1), json!(2.5)] {
            let mut bad = good.clone();
            cell(&mut bad, 2, replacement);
            assert!(parse(&bad, "run", at, at + 30_000).is_err());
        }
        // Reserved above capacity cannot be a real observation.
        let mut bad = good.clone();
        let reserved = bad["data"]["attributes"]["series"]
            .as_array()
            .unwrap()
            .iter()
            .position(|s| s["query_index"] == 8)
            .unwrap();
        cell(&mut bad, reserved, json!(64));
        assert!(parse(&bad, "run", at, at + 30_000).is_err());

        let mut reordered = good.clone();
        for field in ["series", "values"] {
            reordered["data"]["attributes"][field]
                .as_array_mut()
                .unwrap()
                .reverse();
        }
        let e = parse(&reordered, "run", at, at + 30_000).unwrap();
        assert_eq!(e.groups[0].cpu_capacity, 8.);
        let mut duplicate = good.clone();
        duplicate["data"]["attributes"]["series"][3] =
            duplicate["data"]["attributes"]["series"][2].clone();
        assert!(parse(&duplicate, "run", at, at + 30_000).is_err());
        let mut missing = good.clone();
        for field in ["series", "values"] {
            missing["data"]["attributes"][field]
                .as_array_mut()
                .unwrap()
                .pop();
        }
        let error = parse(&missing, "run", at, at + 30_000).unwrap_err();
        assert!(error.starts_with("Warming up"), "{error}");
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
                assert_eq!(qs.len(), QUERIES.len());
                for q in qs {
                    let q = q["query"].as_str().unwrap();
                    assert!(q.starts_with("max:autoscaler."));
                    assert!(q.contains("simulation_run:wire-run"));
                    assert!(q.contains("policy:jev"));
                    assert!(q.contains("env:local"));
                    assert!(q.contains("service:"));
                    assert!(q.ends_with(".rollup(max,10).fill(null)"));
                }
                assert!(qs[6]["query"]
                    .as_str()
                    .unwrap()
                    .contains(" by {group,state}"));
                assert!(!qs[0]["query"].as_str().unwrap().contains(" by "));
                Json(fixture(unix_ms() / 10_000 * 10_000 - 30_000))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let source = Source::for_test(&format!("http://{address}"));
        let e = source
            .fetch_autoscaler("wire-run", unix_ms() - 60_000)
            .await
            .unwrap();
        assert_eq!(e.source, "datadog");
        assert_eq!(e.simulation_run, "wire-run");
        assert!(source
            .fetch_autoscaler("wire-run", unix_ms() + 60_000)
            .await
            .unwrap_err()
            .starts_with("Warming up"));
        assert!(source.fetch_autoscaler("bad run", 0).await.is_err());
        server.abort();
        assert!(source
            .fetch_autoscaler("wire-run", unix_ms() - 60_000)
            .await
            .is_err());
    }
}
