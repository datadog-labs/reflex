//! Two outcome counters and trace/log details. Payloads and free-form messages stay private.
use crate::{
    EvaluationError, ExecutionOutcome, ExecutorError, ProposedDecision, TransitionReceipt,
};
use opentelemetry::{
    metrics::{Counter, Meter},
    KeyValue,
};
use std::{pin::Pin, time::Instant};
use tracing::{field::Empty, Span};

#[derive(Clone)]
pub(crate) struct Telemetry {
    counter: Counter<u64>,
    pub(crate) name: Option<&'static str>,
    label: &'static str,
}
impl Telemetry {
    pub(crate) fn new(meter: Option<Meter>, name: Option<&'static str>, evaluation: bool) -> Self {
        let meter = meter.unwrap_or_else(|| opentelemetry::global::meter("reflex"));
        let (metric, label, description) = if evaluation {
            (
                "reflex.evaluations",
                "controller",
                "Completed evaluations by outcome, including cancellation",
            )
        } else {
            (
                "reflex.transitions",
                "machine",
                "Processed machine inputs by outcome, including effect completion events",
            )
        };
        Self {
            counter: meter
                .u64_counter(metric)
                .with_description(description)
                .build(),
            name,
            label,
        }
    }
    fn count(&self, status: &'static str) {
        let mut attrs = vec![KeyValue::new("status", status)];
        if let Some(name) = self.name {
            attrs.push(KeyValue::new(self.label, name));
        }
        self.counter.add(1, &attrs);
    }
    pub(crate) fn evaluation(&self) -> Operation {
        Operation::new(
            tracing::info_span!("reflex.evaluate", controller = self.name,
            status = Empty, error.type = Empty, error.code = Empty, otel.status_code = Empty),
            Some(self.clone()),
            "evaluation",
        )
    }
    pub(crate) fn execution(&self, id: u64) -> Operation {
        Operation::new(
            tracing::info_span!("reflex.execute", machine = self.name, execution_id = id,
            status = Empty, error.type = Empty, error.code = Empty, otel.status_code = Empty),
            None,
            "execution",
        )
    }
    pub(crate) fn effect(&self, index: usize) -> Operation {
        Operation::new(
            tracing::info_span!("reflex.effect", machine = self.name, effect_index = index,
            status = Empty, error.type = Empty, error.code = Empty, otel.status_code = Empty),
            None,
            "effect",
        )
    }
    pub(crate) fn transition<P>(
        &self,
        result: Result<&ExecutionOutcome<TransitionReceipt<P>>, &ExecutorError>,
        input: &'static str,
        stage: &'static str,
    ) {
        let status = execution_status(result);
        self.count(status);
        match result {
            Ok(ExecutionOutcome::Rejected { reason, .. }) => {
                tracing::debug!(target: "reflex", machine = self.name, status, input_kind = input,
                    stage, reason_code = reason.code.as_str(), "Reflex transition rejected");
            }
            Err(error) => {
                tracing::error!(target: "reflex", machine = self.name, status, input_kind = input,
                    stage, error.type = executor_error(error), "Reflex executor failed");
            }
            _ => {}
        }
    }
}
fn evaluation_error(error: &EvaluationError) -> (&'static str, Option<&str>) {
    match error {
        EvaluationError::Timeout => ("timeout", None),
        EvaluationError::Judge(error) => ("judge_error", Some(error.code.as_str())),
        EvaluationError::InvalidConfidence => ("invalid_confidence", None),
    }
}
fn executor_error(error: &ExecutorError) -> &'static str {
    match error {
        ExecutorError::Poisoned => "poisoned",
        ExecutorError::HookPanicked => "hook_panicked",
        ExecutorError::TaskStopped => "task_stopped",
    }
}
fn execution_status<P>(
    result: Result<&ExecutionOutcome<TransitionReceipt<P>>, &ExecutorError>,
) -> &'static str {
    match result {
        Ok(ExecutionOutcome::Applied(receipt)) if receipt.changed => "applied",
        Ok(ExecutionOutcome::Applied(_)) => "unchanged",
        Ok(ExecutionOutcome::Rejected { .. }) => "rejected",
        Err(_) => "error",
    }
}

/// Owns terminal accounting even when a future is dropped outside its subscriber scope.
pub(crate) struct Operation {
    pub(crate) span: Span,
    dispatch: tracing::Dispatch,
    evaluation: Option<Telemetry>,
    kind: &'static str,
    started: Instant,
    finished: bool,
}
impl Operation {
    fn new(span: Span, evaluation: Option<Telemetry>, kind: &'static str) -> Self {
        Self {
            span,
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            evaluation,
            kind,
            started: Instant::now(),
            finished: false,
        }
    }
    pub(crate) fn finish_evaluation<A>(
        &mut self,
        result: &Result<ProposedDecision<A>, EvaluationError>,
    ) {
        match result {
            Ok(_) => self.finish("proposed"),
            Err(error) => {
                let (kind, code) = evaluation_error(error);
                self.span.record("error.type", kind);
                if let Some(code) = code {
                    self.span.record("error.code", code);
                }
                tracing::warn!(target: "reflex", parent: &self.span, operation = "evaluation", error.type = kind,
                    error.code = code, "Reflex evaluation failed");
                self.finish("error");
            }
        }
    }
    pub(crate) fn finish_execution<P>(
        &mut self,
        result: &Result<ExecutionOutcome<TransitionReceipt<P>>, ExecutorError>,
    ) {
        if let Err(error) = result {
            self.span.record("error.type", executor_error(error));
        }
        self.finish(execution_status(result.as_ref()));
    }
    pub(crate) fn finish(&mut self, status: &'static str) {
        self.span.record("status", status);
        if matches!(status, "error" | "timeout" | "panicked") {
            self.span.record("otel.status_code", "ERROR");
        }
        if let Some(telemetry) = &self.evaluation {
            telemetry.count(status);
        }
        if self.kind == "effect" {
            let duration_s = self.started.elapsed().as_secs_f64();
            if status == "completed" {
                tracing::debug!(target: "reflex", parent: &self.span, status, duration_s, "Reflex effect completed");
            } else {
                self.span.record("error.type", status);
                tracing::warn!(target: "reflex", parent: &self.span, status, duration_s,
                    "Reflex effect did not complete; committed transition remains applied");
            }
        }
        self.finished = true;
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        let dispatch = self.dispatch.clone();
        tracing::dispatcher::with_default(&dispatch, || {
            if !self.finished {
                let error = if std::thread::panicking() {
                    "panicked"
                } else {
                    "cancelled"
                };
                if self.kind != "effect" {
                    self.span.record("error.type", error);
                    tracing::warn!(target: "reflex", parent: &self.span, operation = self.kind,
                        error.type = error, "Reflex operation interrupted");
                }
                // A panic is an evaluation error; ordinary caller cancellation is separate.
                let status = if self.kind == "evaluation" && error == "panicked" {
                    "error"
                } else {
                    error
                };
                self.finish(status);
            }
            self.span = Span::none();
        });
    }
}

// tracing's WithSubscriber scopes polling but not dropping. Judge futures and
// detached tasks may own child spans whose parent references must be released
// under the same dispatcher, including when a caller cancels from another scope.
pin_project_lite::pin_project! {
    pub(crate) struct Scoped<F> {
        #[pin]
        future: Option<F>,
        dispatch: tracing::Dispatch,
    }
    impl<F> PinnedDrop for Scoped<F> {
        fn drop(this: Pin<&mut Self>) {
            let mut this = this.project();
            let _guard = tracing::dispatcher::set_default(this.dispatch);
            this.future.set(None);
        }
    }
}
pub(crate) fn scope<F: std::future::Future>(future: F) -> Scoped<F> {
    Scoped {
        future: Some(future),
        dispatch: tracing::dispatcher::get_default(Clone::clone),
    }
}
impl<F: std::future::Future> std::future::Future for Scoped<F> {
    type Output = F::Output;
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        let _guard = tracing::dispatcher::set_default(this.dispatch);
        this.future
            .as_pin_mut()
            .expect("future is present until drop")
            .poll(cx)
    }
}
