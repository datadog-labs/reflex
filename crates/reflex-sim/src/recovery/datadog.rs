//! Run-scoped recovery observations. Control and action eligibility remain authoritative locally.
use super::judge::{Evaluation, Evaluator, Evidence};
use crate::datadog::{unix_ms, Gauge, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
pub(super) fn new_run() -> String {
    format!(
        "recovery-{}-{}-{}",
        unix_ms(),
        std::process::id(),
        NEXT_RUN.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcomes {
    pub responses: f64,
    pub successes: f64,
    pub failures: f64,
    pub rejected: f64,
    pub success_rate: Option<f64>,
    pub p95_success_latency_ms: Option<f64>,
}
impl Outcomes {
    fn new(values: [f64; 3]) -> Self {
        let responses = values.iter().sum();
        Self {
            responses,
            successes: values[0],
            failures: values[1],
            rejected: values[2],
            success_rate: if responses > 0. {
                Some(values[0] / responses)
            } else {
                None
            },
            p95_success_latency_ms: None,
        }
    }
    fn validate(&self) -> bool {
        [self.responses, self.successes, self.failures, self.rejected]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.)
            && (self.responses - self.successes - self.failures - self.rejected).abs() < 0.001
            && self.success_rate
                == if self.responses > 0. {
                    Some(self.successes / self.responses)
                } else {
                    None
                }
            && self
                .p95_success_latency_ms
                .is_none_or(|v| v.is_finite() && v >= 0.)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicaWindow {
    pub replica: String,
    pub outcomes: Option<Outcomes>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Window {
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
    pub duration_seconds: f64,
    pub all: Outcomes,
    pub essential: Outcomes,
    pub replicas: Vec<ReplicaWindow>,
    pub retry_attempts: Option<f64>,
    pub suppressed_retries: Option<f64>,
    pub rebuild_bytes_per_second: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicaHealth {
    pub replica: String,
    pub outstanding_requests: Gauge,
    pub server_queue_depth: Gauge,
    pub server_active: Gauge,
    pub heartbeat_age_seconds: Gauge,
    pub client_reachable: Gauge,
    pub transfer_reachable: Gauge,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryEvidence {
    pub source: String,
    pub simulation_run: String,
    pub fetched_at_unix_ms: u64,
    pub short_window: Window,
    pub long_window: Window,
    pub replicas: Vec<ReplicaHealth>,
    pub latency_status: String,
}
impl TelemetryEvidence {
    pub fn validate(&self, run: &str, boundary: u64, now: u64) -> Result<(), String> {
        if self.source != "datadog"
            || self.simulation_run != run
            || self.fetched_at_unix_ms > now
            || now - self.fetched_at_unix_ms > 15_000
        {
            return Err("Recovery telemetry query expired or belongs to another run".into());
        }
        for w in [&self.short_window, &self.long_window] {
            if w.start_unix_ms < boundary
                || w.start_unix_ms >= w.end_unix_ms
                || w.end_unix_ms > now
                || now - w.end_unix_ms > 60_000
                || !(30.0..=120.0).contains(&w.duration_seconds)
                || (w.duration_seconds - (w.end_unix_ms - w.start_unix_ms) as f64 / 1000.).abs()
                    > 0.001
                || !w.all.validate()
                || !w.essential.validate()
                || w.essential.responses > w.all.responses + 0.001
                || w.essential.successes > w.all.successes + 0.001
                || w.essential.failures > w.all.failures + 0.001
                || w.essential.rejected > w.all.rejected + 0.001
                || w.replicas.len() != 4
                || w.replicas.iter().enumerate().any(|(i, r)| {
                    r.replica != super::telemetry::replica(i)
                        || r.outcomes.as_ref().is_some_and(|o| !o.validate())
                })
                || [
                    w.retry_attempts,
                    w.suppressed_retries,
                    w.rebuild_bytes_per_second,
                ]
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite() || v < 0.)
            {
                return Err("Recovery telemetry window is stale, incomplete or invalid".into());
            }
        }
        if self.replicas.len() != 4 {
            return Err("Missing replica health observations".into());
        }
        for (i, r) in self.replicas.iter().enumerate() {
            if r.replica != super::telemetry::replica(i) {
                return Err("Invalid replica identity".into());
            }
            for g in [
                &r.outstanding_requests,
                &r.server_queue_depth,
                &r.server_active,
                &r.heartbeat_age_seconds,
                &r.client_reachable,
                &r.transfer_reachable,
            ] {
                if !g.value.is_finite()
                    || g.value < 0.
                    || g.observed_at_unix_ms < boundary
                    || g.observed_at_unix_ms > now
                    || now - g.observed_at_unix_ms > 60_000
                {
                    return Err("Replica observation is missing, invalid or stale".into());
                }
            }
            if ![0., 1.].contains(&r.client_reachable.value)
                || ![0., 1.].contains(&r.transfer_reachable.value)
            {
                return Err("Invalid reachability observation".into());
            }
        }
        Ok(())
    }
}
impl Source {
    #[tracing::instrument(skip_all, name = "datadog.recovery.evidence")]
    pub(crate) async fn fetch_recovery(
        &self,
        run: &str,
        boundary: u64,
    ) -> Result<TelemetryEvidence, String> {
        let service = std::env::var("DD_SERVICE").unwrap_or_else(|_| "reflex".into());
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
        };
        if !valid(run) || !valid(&service) {
            return Err("Invalid recovery telemetry scope".into());
        }
        let now = unix_ms();
        let to = now.saturating_sub(20_000) / 10_000 * 10_000;
        if boundary + 30_000 > to {
            return Err("Warming up: waiting for a complete recovery telemetry window".into());
        }
        let base = format!(
            "env:{},policy:jev,upstream:replica_pool,simulation_run:{run}",
            self.env
        );
        let client = format!("{base},service:{service}");
        let server = format!("{base},service:replica_pool");
        let queries=vec![
            format!("sum:http.client.requests{{{client}}}.as_count().rollup(sum,10)"),
            format!("sum:http.client.requests{{{client}}} by {{essential,outcome}}.as_count().rollup(sum,10)"),
            format!("sum:http.client.requests{{{client},replica:*}} by {{replica,outcome}}.as_count().rollup(sum,10)"),
            format!("sum:http.client.requests{{{client},replica:*}} by {{replica}}.as_count().rollup(sum,10)"),
            format!("sum:recovery.retries{{{client}}} by {{outcome}}.as_count().rollup(sum,10)"),
            format!("sum:recovery.rebuild.bytes{{{client}}}.as_count().rollup(sum,10)"),
            format!("max:recovery.replica.requests.outstanding{{{client}}} by {{replica}}.rollup(max,10).fill(null)"),
            format!("max:http.server.queue.depth{{{server}}} by {{replica}}.rollup(max,10).fill(null)"),
            format!("max:http.server.active{{{server}}} by {{replica}}.rollup(max,10).fill(null)"),
            format!("max:recovery.replica.heartbeat.age{{{client}}} by {{replica}}.rollup(max,10).fill(null)"),
            format!("max:recovery.replica.reachable{{{client}}} by {{replica,path}}.rollup(max,10).fill(null)"),
        ];
        let queries: Vec<_> = queries
            .into_iter()
            .enumerate()
            .map(|(i, q)| json!({"data_source":"metrics","name":format!("q{i}"),"query":q}))
            .collect();
        let value=self.post("/api/v2/query/timeseries",json!({"data":{"type":"timeseries_request","attributes":{
            "from":to.saturating_sub(120_000).max(boundary),"to":to,"interval":10_000,"queries":queries
        }}})).await?;
        let mut evidence = parse(&value, run, boundary, to, unix_ms())?;
        // Scalar percentiles aggregate raw distribution samples across each window.
        let _ = tokio::join!(
            self.recovery_latency(&client, &mut evidence.short_window),
            self.recovery_latency(&client, &mut evidence.long_window)
        );
        let expected: Vec<_> = [&evidence.short_window, &evidence.long_window]
            .into_iter()
            .flat_map(|w| {
                [&w.all, &w.essential]
                    .into_iter()
                    .chain(w.replicas.iter().filter_map(|r| r.outcomes.as_ref()))
            })
            .filter(|o| o.successes > 0.)
            .collect();
        let present = expected
            .iter()
            .filter(|o| o.p95_success_latency_ms.is_some())
            .count();
        evidence.latency_status = if expected.is_empty() {
            "not_applicable"
        } else if present == expected.len() {
            "available"
        } else if present > 0 {
            "partial"
        } else {
            "unavailable"
        }
        .into();
        evidence.validate(run, boundary, unix_ms())?;
        Ok(evidence)
    }
    async fn recovery_latency(&self, scope: &str, w: &mut Window) -> bool {
        let mut targets = vec![
            ("all".to_owned(), String::new()),
            ("essential".into(), ",essential:true".into()),
        ];
        targets.extend((0..4).map(|i| {
            (
                format!("r{i}"),
                format!(",replica:{}", super::telemetry::replica(i)),
            )
        }));
        let queries: Vec<_> = targets
            .into_iter()
            .map(|(name, extra)| {
                json!({"data_source":"metrics","name":name,"aggregator":"percentile",
            "query":format!("p95:http.client.request.duration{{{scope},outcome:success{extra}}}")})
            })
            .collect();
        let Ok(v) = self
            .post(
                "/api/v2/query/scalar",
                json!({"data":{"type":"scalar_request","attributes":{
                    "from":w.start_unix_ms,"to":w.end_unix_ms,"queries":queries
                }}}),
            )
            .await
        else {
            return false;
        };
        let Some(columns) = v
            .pointer("/data/attributes/columns")
            .and_then(Value::as_array)
        else {
            return false;
        };
        let get = |name: &str| {
            columns
                .iter()
                .find(|c| c["name"] == name && c["type"] == "number")
                .and_then(|c| c["values"].as_array())
                .filter(|vs| vs.len() == 1)
                .and_then(|vs| vs[0].as_f64())
                .filter(|v| v.is_finite() && *v >= 0.)
                .map(|v| v * 1000.)
        };
        w.all.p95_success_latency_ms = get("all");
        w.essential.p95_success_latency_ms = get("essential");
        for (i, r) in w.replicas.iter_mut().enumerate() {
            if let Some(o) = &mut r.outcomes {
                o.p95_success_latency_ms = get(&format!("r{i}"));
            }
        }
        w.all.successes == 0. || w.all.p95_success_latency_ms.is_some()
    }
}
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
struct Row {
    q: usize,
    tags: BTreeMap<String, String>,
    values: Vec<Option<f64>>,
}
fn parse(
    v: &Value,
    run: &str,
    boundary: u64,
    to: u64,
    now: u64,
) -> Result<TelemetryEvidence, String> {
    let p: Points = serde_json::from_value(
        v.pointer("/data/attributes")
            .cloned()
            .ok_or("Missing recovery telemetry")?,
    )
    .map_err(|_| "Malformed recovery telemetry")?;
    if p.times.iter().any(|t| *t > to || *t > now)
        || p.times.len() < 3
        || p.series.len() != p.values.len()
        || p.times
            .windows(2)
            .any(|w| w[1].checked_sub(w[0]) != Some(10_000))
    {
        return Err("Invalid recovery telemetry buckets".into());
    }
    let mut rows = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for (s, values) in p.series.into_iter().zip(p.values) {
        let mut tags = BTreeMap::new();
        for tag in s.group_tags {
            let (k, v) = tag.split_once(':').ok_or("Invalid telemetry group")?;
            if tags.insert(k.to_owned(), v.to_owned()).is_some() {
                return Err("Duplicate telemetry group".into());
            }
        }
        let required: &[&str] = match s.query_index {
            0 | 5 => &[],
            1 => &["essential", "outcome"],
            2 => &["replica", "outcome"],
            3 | 6..=9 => &["replica"],
            4 => &["outcome"],
            10 => &["replica", "path"],
            _ => return Err("Unknown telemetry query".into()),
        };
        if tags.len() != required.len()
            || required.iter().any(|k| !tags.contains_key(*k))
            || !seen.insert((s.query_index, tags.clone()))
            || values.len() != p.times.len()
            || values.iter().flatten().any(|v| !v.is_finite() || *v < 0.)
        {
            return Err("Invalid recovery metric series".into());
        }
        if tags
            .get("replica")
            .is_some_and(|r| !(0..4).any(|i| r == super::telemetry::replica(i)))
            || tags
                .get("essential")
                .is_some_and(|e| !matches!(e.as_str(), "true" | "false"))
            || tags.get("outcome").is_some_and(|o| {
                if s.query_index == 4 {
                    !matches!(o.as_str(), "attempted" | "suppressed")
                } else {
                    !matches!(o.as_str(), "success" | "failure" | "rejected")
                }
            })
            || tags
                .get("path")
                .is_some_and(|p| !matches!(p.as_str(), "client" | "transfer"))
        {
            return Err("Unexpected recovery metric tags".into());
        }
        rows.push(Row {
            q: s.query_index,
            tags,
            values,
        });
    }
    let matches = |r: &Row, q: usize, tags: &[(&str, &str)]| {
        r.q == q
            && tags
                .iter()
                .all(|(k, v)| r.tags.get(*k).is_some_and(|s| s == v))
    };
    let get = |q: usize, tags: &[(&str, &str)], i: usize| {
        rows.iter()
            .find(|r| matches(r, q, tags))
            .and_then(|r| r.values[i])
    };
    let last = (0..p.times.len())
        .rev()
        .find(|i| p.times[*i] + 10_000 <= to && get(0, &[], *i).is_some())
        .ok_or("No recent client observations")?;
    let first = last
        .checked_sub(2)
        .ok_or("Incomplete short recovery window")?;
    if p.times[first] < boundary {
        return Err("Warming up: incomplete post-resume recovery window".into());
    }
    let long_first = (0..=first)
        .find(|i| p.times[*i] >= boundary && p.times[last] + 10_000 - p.times[*i] <= 120_000)
        .ok_or("Missing long recovery window")?;
    let sum_matching = |q: usize, tags: &[(&str, &str)], i: usize| {
        rows.iter()
            .filter(|r| matches(r, q, tags))
            .map(|r| r.values[i].unwrap_or(0.))
            .sum::<f64>()
    };
    let make = |start: usize| -> Result<Window, String> {
        let mut all = [0.; 3];
        let mut essential = [0.; 3];
        for i in start..=last {
            let total = get(0, &[], i).ok_or("Gap in recovery client counts")?;
            if (sum_matching(1, &[], i) - total).abs() > 0.001 {
                return Err("Incomplete recovery outcome breakdown".into());
            }
            for (k, outcome) in ["success", "failure", "rejected"].into_iter().enumerate() {
                all[k] += sum_matching(1, &[("outcome", outcome)], i);
                essential[k] += sum_matching(1, &[("outcome", outcome), ("essential", "true")], i);
            }
            if rows
                .iter()
                .filter(|r| r.q == 3)
                .map(|r| r.values[i].unwrap_or(0.))
                .sum::<f64>()
                > total + 0.001
            {
                return Err("Replica counts exceed client total".into());
            }
        }
        let mut replicas = vec![];
        for n in 0..4 {
            let replica = super::telemetry::replica(n);
            let mut vals = [0.; 3];
            let mut complete = true;
            for i in start..=last {
                let Some(total) = get(3, &[("replica", replica)], i) else {
                    complete = false;
                    continue;
                };
                if (total - sum_matching(2, &[("replica", replica)], i)).abs() > 0.001 {
                    return Err("Incomplete per-replica outcome breakdown".into());
                }
                for (k, outcome) in ["success", "failure", "rejected"].into_iter().enumerate() {
                    vals[k] += sum_matching(2, &[("replica", replica), ("outcome", outcome)], i);
                }
            }
            replicas.push(ReplicaWindow {
                replica: replica.into(),
                outcomes: complete.then(|| Outcomes::new(vals)),
            });
        }
        let optional_sum = |q, tags: &[(&str, &str)]| {
            (start..=last)
                .map(|i| get(q, tags, i))
                .collect::<Option<Vec<_>>>()
                .map(|vs| vs.iter().sum::<f64>())
        };
        let seconds = (p.times[last] + 10_000 - p.times[start]) as f64 / 1000.;
        Ok(Window {
            start_unix_ms: p.times[start],
            end_unix_ms: p.times[last] + 10_000,
            duration_seconds: seconds,
            all: Outcomes::new(all),
            essential: Outcomes::new(essential),
            replicas,
            retry_attempts: optional_sum(4, &[("outcome", "attempted")]),
            suppressed_retries: optional_sum(4, &[("outcome", "suppressed")]),
            rebuild_bytes_per_second: optional_sum(5, &[]).map(|v| v / seconds),
        })
    };
    let mut replicas = vec![];
    for n in 0..4 {
        let replica = super::telemetry::replica(n);
        let gauge = |q, extra: Option<&str>| -> Result<Gauge, String> {
            let mut tags = vec![("replica", replica)];
            if let Some(path) = extra {
                tags.push(("path", path));
            }
            let i = (0..=last)
                .rev()
                .find(|i| p.times[*i] >= boundary && get(q, &tags, *i).is_some())
                .ok_or("Missing replica health observation")?;
            Ok(Gauge {
                value: get(q, &tags, i).unwrap(),
                observed_at_unix_ms: p.times[i],
            })
        };
        replicas.push(ReplicaHealth {
            replica: replica.into(),
            outstanding_requests: gauge(6, None)?,
            server_queue_depth: gauge(7, None)?,
            server_active: gauge(8, None)?,
            heartbeat_age_seconds: gauge(9, None)?,
            client_reachable: gauge(10, Some("client"))?,
            transfer_reachable: gauge(10, Some("transfer"))?,
        });
    }
    let e = TelemetryEvidence {
        source: "datadog".into(),
        simulation_run: run.into(),
        fetched_at_unix_ms: now,
        short_window: make(first)?,
        long_window: make(long_first)?,
        replicas,
        latency_status: "unavailable".into(),
    };
    e.validate(run, boundary, now)?;
    Ok(e)
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
    fn evaluate(&self, e: Evidence) -> Evaluation<'_> {
        self.inner.evaluate(e)
    }
}

#[cfg(test)]
fn fixture(end: u64) -> Value {
    let times: Vec<_> = (0..3).map(|i| end - 30_000 + i * 10_000).collect();
    let mut series = vec![];
    let mut values = vec![];
    let mut add = |q: usize, tags: Vec<String>, value: f64| {
        series.push(json!({"query_index":q,"group_tags":tags}));
        values.push(json!([value, value, value]));
    };
    add(0, vec![], 100.);
    for (essential, vs) in [(true, [40., 15., 5.]), (false, [30., 5., 5.])] {
        for (o, v) in ["success", "failure", "rejected"].into_iter().zip(vs) {
            add(
                1,
                vec![format!("essential:{essential}"), format!("outcome:{o}")],
                v,
            );
        }
    }
    for (o, v) in [("success", 20.), ("failure", 10.)] {
        add(
            2,
            vec!["replica:replica_a".into(), format!("outcome:{o}")],
            v,
        );
    }
    add(3, vec!["replica:replica_a".into()], 30.);
    for n in 0..4 {
        let tag = format!("replica:{}", super::telemetry::replica(n));
        for (q, v) in [(6, 8.), (7, 4.), (8, 4.), (9, 0.5)] {
            add(q, vec![tag.clone()], v);
        }
        for path in ["client", "transfer"] {
            add(10, vec![tag.clone(), format!("path:{path}")], 1.);
        }
    }
    json!({"data":{"attributes":{"times":times,"series":series,"values":values}}})
}
#[cfg(test)]
pub(super) fn test_evidence(run: &str, now: u64) -> TelemetryEvidence {
    let end = now / 10_000 * 10_000 - 20_000;
    parse(&fixture(end), run, now - 120_000, end, now).unwrap()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_preserve_unknowns_and_reject_gaps_invalid_breakdowns_and_staleness() {
        let end = 100_000;
        let good = fixture(end);
        let e = parse(&good, "run", 0, end, end + 20_000).unwrap();
        assert_eq!(e.short_window.all.responses, 300.);
        assert_eq!(e.short_window.essential.success_rate, Some(2. / 3.));
        assert_eq!(
            e.short_window.replicas[0]
                .outcomes
                .as_ref()
                .unwrap()
                .failures,
            30.
        );
        assert!(e.short_window.replicas[1].outcomes.is_none());
        assert!(e.short_window.retry_attempts.is_none());
        assert!(e.validate("wrong", 0, end + 20_000).is_err());
        assert!(e.validate("run", end, end + 20_000).is_err());
        assert!(e.validate("run", 0, end + 36_000).is_err());
        let mut stale = e.clone();
        stale.fetched_at_unix_ms = end + 80_000;
        assert!(stale.validate("run", 0, end + 80_000).is_err());
        for (row, change) in [
            (0, Value::Null),
            (1, json!(41)),
            (7, json!(21)),
            (10, Value::Null),
            (10, json!(-1)),
        ] {
            let mut bad = good.clone();
            bad["data"]["attributes"]["values"][row] =
                json!([change.clone(), change.clone(), change]);
            assert!(
                parse(&bad, "run", 0, end, end + 20_000).is_err(),
                "row {row}"
            );
        }
        let mut zero = good.clone();
        for row in zero["data"]["attributes"]["values"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .take(10)
        {
            *row = json!([0., 0., 0.]);
        }
        let e = parse(&zero, "run", 0, end, end + 20_000).unwrap();
        assert_eq!(e.short_window.all.success_rate, None);
        let mut reverse = good.clone();
        reverse["data"]["attributes"]["series"]
            .as_array_mut()
            .unwrap()
            .reverse();
        reverse["data"]["attributes"]["values"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert_eq!(
            parse(&reverse, "run", 0, end, end + 20_000)
                .unwrap()
                .short_window
                .all
                .responses,
            300.
        );
    }
    #[tokio::test]
    async fn wire_queries_scope_health_and_scalar_percentiles() {
        use axum::{http::HeaderMap, routing::post, Json, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app=Router::new().route("/api/v2/query/timeseries",post(|headers:HeaderMap,Json(body):Json<Value>|async move {
            assert_eq!(headers["DD-APPLICATION-KEY"],"test-app");
            let a=&body["data"]["attributes"];let qs=a["queries"].as_array().unwrap();assert_eq!(qs.len(),11);
            for q in qs {let q=q["query"].as_str().unwrap();assert!(q.contains("simulation_run:wire-run"));assert!(q.contains("upstream:replica_pool"));assert!(q.contains("policy:jev"));}
            assert!(qs[7]["query"].as_str().unwrap().contains("service:replica_pool"));
            Json(fixture(a["to"].as_u64().unwrap()))
        })).route("/api/v2/query/scalar",post(|Json(body):Json<Value>|async move {
            for q in body["data"]["attributes"]["queries"].as_array().unwrap(){assert_eq!(q["aggregator"],"percentile");assert!(q["query"].as_str().unwrap().contains("outcome:success"));}
            Json(json!({"data":{"attributes":{"columns":[{"name":"all","type":"number","values":[0.25]}]}}}))
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let source = Source::for_test(&format!("http://{addr}"));
        let e = source
            .fetch_recovery("wire-run", unix_ms() - 120_000)
            .await
            .unwrap();
        assert_eq!(e.short_window.all.p95_success_latency_ms, Some(250.));
        assert_eq!(e.long_window.all.p95_success_latency_ms, Some(250.));
        server.abort();
        assert!(source
            .fetch_recovery("wire-run", unix_ms() - 120_000)
            .await
            .is_err());
    }
}
