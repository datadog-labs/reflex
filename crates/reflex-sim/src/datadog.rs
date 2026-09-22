//! Datadog supplies health evidence; the executor remains authoritative for circuit control.
use crate::{
    jev::{EvaluationFuture, Evaluator, Evidence},
    Error,
};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const TRANSITION_MARGIN_MS: u64 = 20_000;
pub const MAX_AGE_MS: u64 = 60_000;
const BUCKET_MS: u64 = 10_000;
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
    pub duration_seconds: f64,
    pub responses: f64,
    pub successes: f64,
    pub http_errors: f64,
    pub timeouts: f64,
    pub blocked_requests: f64,
    pub failure_ratio: Option<f64>,
    pub p95_latency_ms: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gauge {
    pub value: f64,
    pub observed_at_unix_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub queue_depth: Gauge,
    pub active_requests: Gauge,
    pub utilization: Gauge,
    pub timed_out_work: Gauge,
    pub client_in_flight: Gauge,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvidence {
    pub source: String,
    pub status: String,
    pub simulation_run: String,
    pub fetched_at_unix_ms: u64,
    pub latest_data_age_seconds: f64,
    pub short_window: Window,
    pub long_window: Window,
    pub server: Server,
    pub latency_status: String,
}
impl TelemetryEvidence {
    pub fn validate(&self, now: u64) -> Result<(), &'static str> {
        if self.source != "datadog"
            || self.status != "usable"
            || self.fetched_at_unix_ms > now
            || now - self.fetched_at_unix_ms > 10_000
        {
            return Err("Telemetry is missing or its query result expired");
        }
        for w in [&self.short_window, &self.long_window] {
            if w.start_unix_ms >= w.end_unix_ms
                || w.end_unix_ms > now
                || now - w.end_unix_ms > MAX_AGE_MS
                || w.duration_seconds < 30.0
                || (w.duration_seconds - (w.end_unix_ms - w.start_unix_ms) as f64 / 1000.).abs()
                    > 0.001
            {
                return Err("Telemetry window is stale or incomplete");
            }
            let values = [
                w.responses,
                w.successes,
                w.http_errors,
                w.timeouts,
                w.blocked_requests,
            ];
            if values.iter().any(|v| !v.is_finite() || *v < 0.)
                || (w.responses - w.successes - w.http_errors - w.timeouts).abs() > 0.001
            {
                return Err("Inconsistent request counts");
            }
            let expected = if w.responses > 0. {
                Some((w.http_errors + w.timeouts) / w.responses)
            } else {
                None
            };
            if w.failure_ratio != expected
                || w.p95_latency_ms.is_some_and(|v| !v.is_finite() || v < 0.)
            {
                return Err("Invalid request statistics");
            }
        }
        for g in [
            &self.server.queue_depth,
            &self.server.active_requests,
            &self.server.utilization,
            &self.server.timed_out_work,
            &self.server.client_in_flight,
        ] {
            if !g.value.is_finite()
                || g.value < 0.
                || g.observed_at_unix_ms > now
                || now - g.observed_at_unix_ms > MAX_AGE_MS
            {
                return Err("Server measurement is invalid or stale");
            }
        }
        if self.server.utilization.value > 1. {
            return Err("Invalid utilization");
        }
        Ok(())
    }
}

pub struct Source {
    client: reqwest::Client,
    base: String,
    pub(crate) env: String,
}
impl Source {
    #[cfg(test)]
    pub(crate) fn for_test(base: &str) -> Self {
        Self::new(base, "test-api", "test-app", "local").unwrap()
    }
    pub fn from_env() -> Result<Self, Error> {
        let key = std::env::var("DD_API_KEY")
            .map_err(|_| Error::Invalid("DD_API_KEY is required".into()))?;
        let app = std::env::var("DD_APP_KEY").map_err(|_| {
            Error::Invalid(
                "DD_APP_KEY with timeseries_query permission is required for Datadog evidence"
                    .into(),
            )
        })?;
        let site = std::env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".into());
        if ![
            "datadoghq.com",
            "datadoghq.eu",
            "us3.datadoghq.com",
            "us5.datadoghq.com",
            "ap1.datadoghq.com",
            "ap2.datadoghq.com",
            "uk1.datadoghq.com",
        ]
        .contains(&site.as_str())
        {
            return Err(Error::Invalid("Unsupported DD_SITE".into()));
        }
        Self::new(
            &format!("https://api.{site}"),
            &key,
            &app,
            &std::env::var("DD_ENV").unwrap_or_else(|_| "local".into()),
        )
    }
    fn new(base: &str, key: &str, app: &str, env: &str) -> Result<Self, Error> {
        if !tag(env) || key.trim().is_empty() || app.trim().is_empty() {
            return Err(Error::Invalid(
                "Invalid Datadog credentials or DD_ENV".into(),
            ));
        }
        let mut headers = HeaderMap::new();
        for (name, value) in [("DD-API-KEY", key), ("DD-APPLICATION-KEY", app)] {
            let mut value = HeaderValue::from_str(value)
                .map_err(|_| Error::Invalid("Invalid Datadog credential header".into()))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(4))
            .build()
            .map_err(|_| Error::Invalid("Could not create Datadog HTTP client".into()))?;
        Ok(Self {
            client,
            base: base.into(),
            env: env.into(),
        })
    }
    pub(crate) async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|_| "Datadog query timed out or could not connect".to_owned())?;
        if !response.status().is_success() {
            return Err(format!(
                "Datadog query returned HTTP {}",
                response.status().as_u16()
            ));
        }
        // Bound the response before parsing; never expose provider bodies or credentials in errors.
        if response.content_length().is_some_and(|n| n > 2_000_000) {
            return Err("Datadog response was too large".into());
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "Could not read Datadog response")?;
        if bytes.len() > 2_000_000 {
            return Err("Datadog response was too large".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Datadog JSON")?;
        if value
            .get("errors")
            .is_some_and(|e| !e.is_null() && e != &json!([]) && e != &json!(""))
        {
            return Err("Datadog could not evaluate the metric queries".into());
        }
        Ok(value)
    }
    fn scope(&self, service: &str, run: &str, server: bool) -> String {
        format!(
            "env:{},policy:jev,simulation_run:{},{}:{}",
            self.env,
            run,
            if server { "service" } else { "upstream" },
            service
        )
    }
    #[tracing::instrument(skip_all, fields(upstream = service), name = "datadog.evidence")]
    pub(crate) async fn fetch(
        &self,
        service: &str,
        run: &str,
        not_before: u64,
    ) -> Result<TelemetryEvidence, String> {
        if !tag(service) || !tag(run) {
            return Err("Invalid telemetry scope".into());
        }
        let now = unix_ms();
        let to = now.saturating_sub(20_000) / BUCKET_MS * BUCKET_MS;
        let from = to.saturating_sub(120_000);
        if not_before.saturating_add(30_000) > to {
            return Err("Warming up: waiting for a complete post-transition Datadog window".into());
        }
        let scope = self.scope(service, run, false);
        let server = self.scope(service, run, true);
        let mut queries = vec![];
        // Total plus the four exhaustive outcomes detects missing outcome data without inventing zeros.
        for (i, filter) in [
            "",
            "outcome:success",
            "outcome:http_error",
            "outcome:timeout",
            "outcome:circuit_open",
        ]
        .iter()
        .enumerate()
        {
            let filter = if filter.is_empty() {
                scope.clone()
            } else {
                format!("{scope},{filter}")
            };
            queries.push(json!({"data_source":"metrics","name":format!("q{i}"),"query":format!("sum:http.client.requests{{{filter}}}.as_count().rollup(sum,10)")}));
        }
        for (i, (metric, scope)) in [
            ("http.server.queue.depth", &server),
            ("http.server.active", &server),
            ("http.server.utilization", &server),
            ("http.server.timed_out_work", &server),
            ("http.client.in_flight", &scope),
        ]
        .iter()
        .enumerate()
        {
            queries.push(json!({"data_source":"metrics","name":format!("q{}",i+5),"query":format!("max:{metric}{{{scope}}}.rollup(max,10).fill(null)")}));
        }
        let body = json!({"data":{"type":"timeseries_request","attributes":{"from":from,"to":to,"interval":BUCKET_MS,"queries":queries}}});
        let value = self.post("/api/v2/query/timeseries", body).await?;
        let mut result = parse(&value, not_before, to, now, run)?;
        let (short, long) = tokio::join!(
            self.percentile(service, run, &result.short_window),
            self.percentile(service, run, &result.long_window)
        );
        result.short_window.p95_latency_ms = short;
        result.long_window.p95_latency_ms = long;
        result.latency_status = if short.is_some() && long.is_some() {
            "available"
        } else {
            "unavailable: percentile queries returned no usable value"
        }
        .into();
        result.fetched_at_unix_ms = unix_ms();
        result.latest_data_age_seconds = result
            .fetched_at_unix_ms
            .saturating_sub(result.short_window.end_unix_ms)
            as f64
            / 1000.;
        result.validate(result.fetched_at_unix_ms)?;
        Ok(result)
    }
    async fn percentile(&self, service: &str, run: &str, w: &Window) -> Option<f64> {
        if w.responses == 0. {
            return None;
        }
        let scope = self.scope(service, run, false).replace(',', " AND ");
        let body = json!({"data":{"type":"scalar_request","attributes":{"from":w.start_unix_ms,"to":w.end_unix_ms,"queries":[{"data_source":"metrics","name":"latency","aggregator":"percentile","query":format!("p95:http.client.request.duration{{{scope} AND NOT outcome:circuit_open}}")}]}}});
        let value = self.post("/api/v2/query/scalar", body).await.ok()?;
        let columns = value.pointer("/data/attributes/columns")?.as_array()?;
        let column = columns
            .iter()
            .find(|c| c["name"] == "latency" && c["type"] == "number")?;
        let values = column["values"].as_array()?;
        if values.len() != 1 {
            return None;
        }
        let value = values[0].as_f64()?;
        (value.is_finite() && value >= 0.).then_some(value * 1000.)
    }
}
fn tag(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-./".contains(&c))
}
#[derive(Deserialize)]
struct Series {
    query_index: usize,
}
#[derive(Deserialize)]
struct Points {
    times: Vec<u64>,
    series: Vec<Series>,
    values: Vec<Vec<Option<f64>>>,
}
fn parse(
    value: &Value,
    not_before: u64,
    to: u64,
    now: u64,
    run: &str,
) -> Result<TelemetryEvidence, String> {
    let p: Points = serde_json::from_value(
        value
            .pointer("/data/attributes")
            .cloned()
            .ok_or("Missing Datadog data")?,
    )
    .map_err(|_| "Malformed Datadog timeseries")?;
    if p.times.len() < 2 || p.series.len() != p.values.len() {
        return Err("Datadog has not returned enough observations".into());
    }
    let width = p.times[1]
        .checked_sub(p.times[0])
        .filter(|n| *n >= BUCKET_MS && *n <= 30_000)
        .ok_or("Unsupported Datadog rollup")?;
    if p.times
        .windows(2)
        .any(|w| w[1].checked_sub(w[0]) != Some(width))
    {
        return Err("Irregular Datadog timestamps".into());
    }
    let mut rows = vec![vec![None; p.times.len()]; 10];
    let mut seen = [false; 10];
    for (s, values) in p.series.iter().zip(p.values) {
        if s.query_index >= 10
            || seen[s.query_index]
            || values.len() != p.times.len()
            || values.iter().flatten().any(|v| !v.is_finite() || *v < 0.)
        {
            return Err("Unexpected or invalid Datadog series".into());
        }
        seen[s.query_index] = true;
        rows[s.query_index] = values;
    }
    let last = (0..p.times.len())
        .rev()
        .find(|i| p.times[*i].saturating_add(width) <= to && rows[0][*i].is_some())
        .ok_or("No recent request observations")?;
    let end = p.times[last] + width;
    let short_bins = 30_000u64.div_ceil(width) as usize;
    let first = last
        .checked_add(1)
        .and_then(|n| n.checked_sub(short_bins))
        .ok_or("Incomplete short window")?;
    if p.times[first] < not_before {
        return Err("Warming up: post-transition window is incomplete".into());
    }
    let long_first = (0..=first)
        .find(|i| p.times[*i] >= not_before && end - p.times[*i] <= 120_000)
        .ok_or("Incomplete long window")?;
    let make = |start: usize| -> Result<Window, String> {
        let mut sum = [0.; 4];
        for (i, total) in rows[0].iter().enumerate().take(last + 1).skip(start) {
            let total = total.ok_or("Gap in request observations")?;
            let outcomes = [
                rows[1][i].unwrap_or(0.),
                rows[2][i].unwrap_or(0.),
                rows[3][i].unwrap_or(0.),
                rows[4][i].unwrap_or(0.),
            ];
            if (total - outcomes.iter().sum::<f64>()).abs() > 0.001 {
                return Err("Incomplete outcome breakdown".into());
            }
            for (a, b) in sum.iter_mut().zip(outcomes) {
                *a += b;
            }
        }
        let responses = sum[0] + sum[1] + sum[2];
        Ok(Window {
            start_unix_ms: p.times[start],
            end_unix_ms: end,
            duration_seconds: (end - p.times[start]) as f64 / 1000.,
            responses,
            successes: sum[0],
            http_errors: sum[1],
            timeouts: sum[2],
            blocked_requests: sum[3],
            failure_ratio: if responses > 0. {
                Some((sum[1] + sum[2]) / responses)
            } else {
                None
            },
            p95_latency_ms: None,
        })
    };
    let gauge = |row: usize| -> Result<Gauge, String> {
        let i = (0..=last)
            .rev()
            .find(|i| rows[row][*i].is_some() && p.times[*i] >= not_before)
            .ok_or("Missing server measurement")?;
        Ok(Gauge {
            value: rows[row][i].unwrap(),
            observed_at_unix_ms: p.times[i],
        })
    };
    let result = TelemetryEvidence {
        source: "datadog".into(),
        status: "usable".into(),
        simulation_run: run.into(),
        fetched_at_unix_ms: now,
        latest_data_age_seconds: now.saturating_sub(end) as f64 / 1000.,
        short_window: make(first)?,
        long_window: make(long_first)?,
        server: Server {
            queue_depth: gauge(5)?,
            active_requests: gauge(6)?,
            utilization: gauge(7)?,
            timed_out_work: gauge(8)?,
            client_in_flight: gauge(9)?,
        },
        latency_status: "unavailable".into(),
    };
    result.validate(now)?;
    Ok(result)
}
/// Selects Datadog input for the playground without installing providers or changing the judge SDK.
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
    fn evaluate(&self, evidence: Evidence) -> EvaluationFuture<'_> {
        self.inner.evaluate(evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(to: u64) -> Value {
        let times: Vec<_> = (0..13).map(|i| to - 120_000 + i * 10_000).collect();
        let values: Vec<Vec<Option<f64>>> = [100., 80., 10., 5., 5., 24., 8., 1., 3., 9.]
            .into_iter()
            .map(|v| vec![Some(v); times.len()])
            .collect();
        json!({"data":{"type":"timeseries_response","attributes":{"times":times,"series":(0..10).map(|i|json!({"query_index":i})).collect::<Vec<_>>(),"values":values}}})
    }
    #[test]
    fn windows_exclude_blocked_requests_and_reject_gaps_staleness_and_old_generations() {
        let to = 1_000_000;
        let data = fixture(to);
        let e = parse(&data, to - 120_000, to, to + 20_000, "run-a").unwrap();
        assert_eq!(e.short_window.responses, 285.);
        assert_eq!(e.short_window.blocked_requests, 15.);
        assert_eq!(e.short_window.failure_ratio, Some(45. / 285.));
        assert_eq!(e.long_window.responses, 1140.);
        assert_eq!(e.server.queue_depth.observed_at_unix_ms, to - 10_000);
        assert!(parse(&data, to - 20_000, to, to + 20_000, "run-a").is_err());
        assert!(parse(&data, to - 120_000, to, to + 70_000, "run-a").is_err());
        let mut missing = data.clone();
        missing["data"]["attributes"]["values"][0][10] = Value::Null;
        assert!(parse(&missing, to - 120_000, to, to + 20_000, "run-a").is_err());
        let mut missing = data.clone();
        missing["data"]["attributes"]["values"][2][10] = Value::Null;
        assert!(parse(&missing, to - 120_000, to, to + 20_000, "run-a").is_err());
        let mut stale_gauge = data.clone();
        stale_gauge["data"]["attributes"]["values"][5] =
            json!([24., null, null, null, null, null, null, null, null, null, null, null, null]);
        assert!(parse(&stale_gauge, to - 120_000, to, to + 20_000, "run-a").is_err());
        let mut e = e;
        e.fetched_at_unix_ms = to;
        assert!(e.validate(to + 20_000).is_err());
    }
    #[test]
    fn absent_outcome_is_zero_only_when_totals_prove_the_breakdown_complete() {
        let to = 1_000_000;
        let mut data = fixture(to);
        data["data"]["attributes"]["values"][0] = json!(vec![80.; 13]);
        for i in 2..5 {
            data["data"]["attributes"]["values"][i] = json!(vec![None::<f64>; 13]);
        }
        let e = parse(&data, to - 120_000, to, to + 20_000, "run-a").unwrap();
        assert_eq!(e.short_window.failure_ratio, Some(0.));
        for i in 0..2 {
            data["data"]["attributes"]["values"][i] = json!(vec![0.; 13]);
        }
        let e = parse(&data, to - 120_000, to, to + 20_000, "run-a").unwrap();
        assert_eq!(e.short_window.failure_ratio, None);
        data["data"]["attributes"]["values"][0] = json!(vec![None::<f64>; 13]);
        assert!(parse(&data, to - 120_000, to, to + 20_000, "run-a").is_err());
    }
    #[test]
    fn response_series_order_is_not_query_order_and_invalid_values_fail_closed() {
        let to = 1_000_000;
        let mut data = fixture(to);
        data["data"]["attributes"]["series"]
            .as_array_mut()
            .unwrap()
            .reverse();
        data["data"]["attributes"]["values"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert_eq!(
            parse(&data, to - 120_000, to, to + 20_000, "run-a")
                .unwrap()
                .short_window
                .responses,
            285.
        );
        data["data"]["attributes"]["values"][0][10] = json!(-1.);
        assert!(parse(&data, to - 120_000, to, to + 20_000, "run-a").is_err());
    }
    #[tokio::test]
    async fn query_wire_contract_authentication_percentiles_and_unavailable_provider() {
        use axum::{
            extract::State,
            http::{HeaderMap, StatusCode},
            routing::post,
            Json, Router,
        };
        use std::sync::Mutex;
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app=Router::new().route("/api/v2/query/timeseries",post(|State(c):State<Arc<Mutex<Vec<Value>>>>,h:HeaderMap,Json(body):Json<Value>| async move {
            assert_eq!(h["dd-api-key"],"test-api"); assert_eq!(h["dd-application-key"],"test-app");
            c.lock().unwrap().push(body.clone());
            Json(fixture(body["data"]["attributes"]["to"].as_u64().unwrap()))
        })).route("/api/v2/query/scalar",post(|State(c):State<Arc<Mutex<Vec<Value>>>>,Json(body):Json<Value>|async move {
            c.lock().unwrap().push(body);
            Json(json!({"data":{"attributes":{"columns":[{"type":"number","name":"latency","values":[0.65]}]}}}))
        })).route("/bad/api/v2/query/timeseries",post(||async {StatusCode::TOO_MANY_REQUESTS})).with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let source = Source::new(&base, "test-api", "test-app", "local").unwrap();
        let e = source
            .fetch("payments", "run-a", unix_ms() - 180_000)
            .await
            .unwrap();
        assert_eq!(e.short_window.p95_latency_ms, Some(650.));
        assert_eq!(e.long_window.p95_latency_ms, Some(650.));
        let requests = captured.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        let queries = requests[0]["data"]["attributes"]["queries"]
            .as_array()
            .unwrap();
        for q in queries {
            let text = q["query"].as_str().unwrap();
            assert!(text.contains("simulation_run:run-a"));
            assert!(text.contains("env:local"));
            assert!(text.contains("policy:jev"));
        }
        assert!(queries[0]["query"]
            .as_str()
            .unwrap()
            .contains("upstream:payments"));
        assert!(queries[5]["query"]
            .as_str()
            .unwrap()
            .contains("service:payments"));
        for request in &requests[1..] {
            assert_eq!(
                request["data"]["attributes"]["queries"][0]["aggregator"],
                "percentile"
            );
            assert!(request.to_string().contains("NOT outcome:circuit_open"));
        }
        let source = Source::new(&format!("{base}/bad"), "test-api", "test-app", "local").unwrap();
        assert_eq!(
            source
                .fetch("payments", "run-a", unix_ms() - 180_000)
                .await
                .unwrap_err(),
            "Datadog query returned HTTP 429"
        );
        task.abort();
    }
}
