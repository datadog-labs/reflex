//! Internal request accounting. No payloads, credentials, or free-form errors are recorded.
use crate::Error;
use opentelemetry::{
    metrics::{Counter, Histogram, Meter, ObservableGauge},
    KeyValue,
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tracing::{field::Empty, Span};

static ACTIVE_CALLS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) struct Telemetry {
    requests: Counter<u64>,
    call_duration: Histogram<f64>,
    request_duration: Histogram<f64>,
    backoff_duration: Histogram<f64>,
    tokens: Counter<u64>,
    // A gauge works with direct Datadog intake, which does not accept cumulative sums.
    _in_flight: ObservableGauge<u64>,
}
impl Telemetry {
    pub(crate) fn new(meter: Meter) -> Self {
        Self {
            requests: meter
                .u64_counter("typesafe.client.requests")
                .with_description(
                    "Finished HTTP attempts, including retries and interrupted attempts",
                )
                .build(),
            call_duration: meter
                .f64_histogram("typesafe.client.call.duration")
                .with_unit("s")
                .with_boundaries(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0,
                    60.0,
                ])
                .with_description(
                    "Complete call latency including preparation, retries and backoff",
                )
                .build(),
            request_duration: meter
                .f64_histogram("typesafe.client.request.duration")
                .with_unit("s")
                .with_boundaries(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0,
                    60.0,
                ])
                .with_description(
                    "HTTP attempt latency including body reading and validation, excluding backoff",
                )
                .build(),
            backoff_duration: meter
                .f64_histogram("typesafe.client.retry.backoff.duration")
                .with_unit("s")
                .with_boundaries(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0,
                    60.0,
                ])
                .with_description("Actual retry wait, including interrupted waits")
                .build(),
            tokens: meter
                .u64_counter("typesafe.client.tokens")
                .with_description(
                    "Validated provider-reported token usage; missing usage is not zero",
                )
                .build(),
            _in_flight: meter
                .u64_observable_gauge("typesafe.client.calls.in_flight")
                .with_description("Active TypeSafe calls in this process, across all clients")
                .with_callback(|observer| {
                    observer.observe(ACTIVE_CALLS.load(Ordering::Relaxed), &[])
                })
                .build(),
        }
    }
    pub(crate) fn start(&self, model: &str, timeout: Duration, retries: u32) -> Call {
        let span = tracing::info_span!("typesafe.system_one",
            otel.kind = "client", model.requested = model, model.resolved = Empty,
            timeout_s = timeout.as_secs_f64(), max_retries = retries,
            stage = Empty, status = Empty, error.type = Empty, error.stage = Empty,
            attempt_count = Empty, request_id = Empty, http_status = Empty,
            input_tokens = Empty, output_tokens = Empty, retries_exhausted = Empty,
            otel.status_code = Empty);
        ACTIVE_CALLS.fetch_add(1, Ordering::Relaxed);
        Call {
            telemetry: self.clone(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            model: model.to_owned(),
            span,
            started: Instant::now(),
            stage: "preparing",
            attempts: 0,
            http_status: None,
            request_id: None,
            retries_exhausted: None,
            attempt: None,
            backoff: None,
            finished: false,
        }
    }
}
struct Attempt {
    started: Instant,
    span: Span,
    http_status: Option<u16>,
    retry: bool,
}
pub(crate) struct Call {
    telemetry: Telemetry,
    dispatch: tracing::Dispatch,
    model: String,
    pub(crate) span: Span,
    started: Instant,
    stage: &'static str,
    attempts: u32,
    http_status: Option<u16>,
    request_id: Option<String>,
    pub(crate) retries_exhausted: Option<bool>,
    attempt: Option<Attempt>,
    backoff: Option<Instant>,
    finished: bool,
}
impl Call {
    pub(crate) fn stage(&mut self, stage: &'static str) {
        self.stage = stage;
        tracing::debug!(target: "typesafe_ai", parent: &self.span, stage,
            attempt_count = self.attempts, "TypeSafe request stage");
    }
    pub(crate) fn begin_attempt(&mut self) -> Span {
        self.attempts += 1;
        self.http_status = None;
        self.request_id = None;
        let span = tracing::info_span!(parent: &self.span, "typesafe.http_attempt",
            otel.kind = "client", http.request.method = "POST", attempt = self.attempts,
            retry = self.attempts > 1, http_status = Empty, request_id = Empty,
            stage = Empty, status = Empty, error.type = Empty, error.stage = Empty,
            otel.status_code = Empty);
        self.attempt = Some(Attempt {
            started: Instant::now(),
            span: span.clone(),
            http_status: None,
            retry: self.attempts > 1,
        });
        self.stage("sending");
        span
    }
    pub(crate) fn response(&mut self, status: u16, request_id: Option<&str>) {
        self.http_status = Some(status);
        self.request_id = request_id.map(str::to_owned);
        if let Some(attempt) = &mut self.attempt {
            attempt.http_status = Some(status);
            attempt.span.record("http_status", status);
            if let Some(id) = request_id {
                attempt.span.record("request_id", id);
            }
        }
    }
    pub(crate) fn end_attempt(&mut self, status: &'static str, error: Option<&'static str>) {
        if let Some(attempt) = self.attempt.take() {
            attempt.span.record("status", status);
            attempt.span.record("stage", self.stage);
            let mut attrs = vec![
                KeyValue::new("model", self.model.clone()),
                KeyValue::new("status", status),
                KeyValue::new("retry", attempt.retry),
            ];
            if let Some(code) = attempt.http_status {
                attrs.push(KeyValue::new("http_status", i64::from(code)));
            }
            if let Some(error) = error {
                attrs.push(KeyValue::new("error.type", error));
                attempt.span.record("error.type", error);
                attempt.span.record("error.stage", self.stage);
                if status != "cancelled" {
                    attempt.span.record("otel.status_code", "ERROR");
                }
            }
            self.telemetry.requests.add(1, &attrs);
            self.telemetry
                .request_duration
                .record(attempt.started.elapsed().as_secs_f64(), &attrs);
        }
    }
    pub(crate) fn begin_backoff(&mut self, delay: Duration, status: u16) {
        self.stage("backing_off");
        self.backoff = Some(Instant::now());
        tracing::warn!(target: "typesafe_ai", parent: &self.span, http_status = status,
            model = self.model.as_str(), request_id = self.request_id.as_deref(),
            attempt = self.attempts, retry_delay_s = delay.as_secs_f64(), "TypeSafe retry scheduled");
    }
    pub(crate) fn end_backoff(&mut self, status: &'static str) {
        if let Some(started) = self.backoff.take() {
            self.telemetry.backoff_duration.record(
                started.elapsed().as_secs_f64(),
                &[
                    KeyValue::new("model", self.model.clone()),
                    KeyValue::new("status", status),
                ],
            );
        }
    }
    pub(crate) fn usage(&self, model: &str, input: u64, output: u64) {
        self.span.record("model.resolved", model);
        self.span.record("input_tokens", input);
        self.span.record("output_tokens", output);
        for (direction, count) in [("input", input), ("output", output)] {
            self.telemetry.tokens.add(
                count,
                &[
                    KeyValue::new("model", model.to_owned()),
                    KeyValue::new("direction", direction),
                ],
            );
        }
    }
    pub(crate) fn finish<T>(&mut self, result: &Result<T, Error>) {
        let (status, error) = match result {
            Ok(_) => ("success", None),
            Err(Error::Configuration(_)) => ("configuration_error", Some("configuration")),
            Err(Error::Json(_)) if self.stage == "preparing" => {
                ("serialization_error", Some("serialization"))
            }
            Err(Error::Timeout) => ("timeout", Some("timeout")),
            Err(Error::Http { .. }) => ("http_error", Some("http")),
            Err(Error::Transport(_)) => ("transport_error", Some("transport")),
            Err(_) => ("invalid_response", Some("invalid_response")),
        };
        self.complete(status, error);
    }
    fn complete(&mut self, status: &'static str, error: Option<&'static str>) {
        self.end_attempt(status, error);
        self.end_backoff(status);
        // Record mutable summary fields once: some tracing bridges append repeated
        // attributes instead of replacing them, producing ambiguous OTLP keys.
        self.span.record("status", status);
        self.span.record("stage", self.stage);
        self.span.record("attempt_count", self.attempts);
        if let Some(code) = self.http_status {
            self.span.record("http_status", code);
        }
        if let Some(id) = &self.request_id {
            self.span.record("request_id", id.as_str());
        }
        if let Some(exhausted) = self.retries_exhausted {
            self.span.record("retries_exhausted", exhausted);
        }
        let mut attrs = vec![
            KeyValue::new("model", self.model.clone()),
            KeyValue::new("status", status),
        ];
        if let Some(error) = error {
            self.span.record("error.type", error);
            self.span.record("error.stage", self.stage);
            if status != "cancelled" {
                self.span.record("otel.status_code", "ERROR");
            }
            attrs.push(KeyValue::new("error.type", error));
            attrs.push(KeyValue::new("error.stage", self.stage));
            tracing::warn!(target: "typesafe_ai", parent: &self.span, status, error.type = error,
                error.stage = self.stage, attempt_count = self.attempts,
                model = self.model.as_str(), request_id = self.request_id.as_deref(),
                http_status = self.http_status, retries_exhausted = self.retries_exhausted,
                "TypeSafe call did not complete successfully");
        }
        self.telemetry
            .call_duration
            .record(self.started.elapsed().as_secs_f64(), &attrs);
        ACTIVE_CALLS.fetch_sub(1, Ordering::Relaxed);
        self.finished = true;
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        // A caller can drop a future outside the subscriber scope in which it was
        // polled. Restore that dispatcher for terminal events and parent-span release.
        let dispatch = self.dispatch.clone();
        tracing::dispatcher::with_default(&dispatch, || {
            if !self.finished {
                let reason = if std::thread::panicking() {
                    "panicked"
                } else {
                    "cancelled"
                };
                self.complete(reason, Some(reason));
            }
            self.span = Span::none();
        });
    }
}
