// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Runtime-only context carried across the evaluator-to-simulation-clock handoff.
//! It never enters model evidence, recorded decisions, or replay data.
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tracing::{field::Empty, instrument::Instrumented, Dispatch, Instrument, Span};

pub(crate) struct DecisionTrace {
    span: Span,
    dispatch: Dispatch,
    finished: bool,
    kind: &'static str,
}
impl DecisionTrace {
    pub fn new(upstream: &str, id: u64, state: &str, simulation_time_ms: f64) -> Self {
        Self {
            span: tracing::info_span!(target: "reflex_sim::circuit_breaker", "circuit_breaker.decision",
                upstream, decision_id = id, state, simulation_time_ms, status = Empty, otel.status_code = Empty),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            finished: false,
            kind: "circuit_breaker",
        }
    }
    pub fn scheduler(job: u64, client: u64, policy: &'static str, simulation_time_ms: u64) -> Self {
        let client = format!("client_{}", client + 1);
        Self {
            span: tracing::info_span!(target: "reflex_sim::scheduler", "scheduler.decision",
                job_id = job, client, policy, simulation_time_ms, status = Empty, otel.status_code = Empty),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            finished: false,
            kind: "scheduler",
        }
    }
    pub fn scheduler_selection(&self, job: u64, client: u64) {
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span.record("job_id", job);
            self.span.record("client", format!("client_{}", client + 1));
        });
    }
    pub fn child(&self, stage: &str) -> Span {
        tracing::dispatcher::with_default(&self.dispatch, || {
            if self.kind == "scheduler" && stage == "evaluate" {
                tracing::info_span!(target: "reflex_sim::scheduler", parent: &self.span, "scheduler.evaluate")
            } else if self.kind == "scheduler" {
                tracing::info_span!(target: "reflex_sim::scheduler", parent: &self.span, "scheduler.apply")
            } else if stage == "evaluate" {
                tracing::info_span!(target: "reflex_sim::circuit_breaker", parent: &self.span, "circuit_breaker.evaluate")
            } else {
                tracing::info_span!(target: "reflex_sim::circuit_breaker", parent: &self.span, "circuit_breaker.apply")
            }
        })
    }
    pub fn scope<F: Future>(&self, future: F) -> Scoped<Instrumented<F>> {
        Scoped {
            future: Some(future.instrument(self.span.clone())),
            dispatch: self.dispatch.clone(),
        }
    }
    pub fn finish(&mut self, status: &str) {
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span.record("status", status);
            if matches!(status, "error" | "evaluation_error") {
                self.span.record("otel.status_code", "ERROR");
            }
        });
        self.finished = true;
    }
}
impl Drop for DecisionTrace {
    fn drop(&mut self) {
        if !self.finished {
            self.finish("cancelled");
        }
        // A reset may discard a pending result outside the original subscriber scope.
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span = Span::none();
        });
    }
}

pin_project_lite::pin_project! {
    // WithSubscriber only scopes polling. Scope cancellation/drop as well, so child spans
    // release their parent references even when a Tokio task is aborted on another worker.
    pub(crate) struct Scoped<F> {
        #[pin]
        future: Option<F>,
        dispatch: Dispatch,
    }
    impl<F> PinnedDrop for Scoped<F> {
        fn drop(this: Pin<&mut Self>) {
            let mut this = this.project();
            let _guard = tracing::dispatcher::set_default(this.dispatch);
            this.future.set(None);
        }
    }
}
impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _guard = tracing::dispatcher::set_default(this.dispatch);
        this.future
            .as_pin_mut()
            .expect("future exists until drop")
            .poll(cx)
    }
}
