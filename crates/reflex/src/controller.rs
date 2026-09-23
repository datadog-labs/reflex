// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::telemetry::{scope, Telemetry};
use opentelemetry::metrics::Meter;
use std::{future::Future, time::Duration};
use thiserror::Error;
use tracing::Instrument;

/// A heuristic result. Confidence is not a probability of operational success.
#[derive(Debug, Clone)]
pub struct Judgment<A> {
    pub action: A,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {message}")]
pub struct JudgeError {
    pub code: String,
    pub message: String,
}
impl JudgeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Implement with `async fn judge`. Returned futures must be Send.
pub trait Judge<S, A>: Send + Sync {
    fn judge(&self, state: &S) -> impl Future<Output = Result<Judgment<A>, JudgeError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvaluationError {
    #[error("inference deadline exceeded")]
    Timeout,
    #[error(transparent)]
    Judge(#[from] JudgeError),
    #[error("confidence must be finite and between zero and one")]
    InvalidConfidence,
}

#[derive(Debug, Clone)]
pub struct ProposedDecision<A> {
    action: A,
    confidence: Option<f64>,
}
impl<A> ProposedDecision<A> {
    pub fn action(&self) -> &A {
        &self.action
    }
    pub fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}
impl<A> TryFrom<Judgment<A>> for ProposedDecision<A> {
    type Error = EvaluationError;
    fn try_from(value: Judgment<A>) -> Result<Self, Self::Error> {
        if value.confidence.is_some_and(|v| !valid_confidence(v)) {
            return Err(EvaluationError::InvalidConfidence);
        }
        Ok(Self {
            action: value.action,
            confidence: value.confidence,
        })
    }
}
pub(crate) fn valid_confidence(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

#[derive(Debug, Error)]
#[error("inference timeout must be greater than zero")]
pub struct ControllerConfigError;

pub struct Controller<J> {
    judge: J,
    timeout: Duration,
    telemetry: Telemetry,
}
pub struct ControllerBuilder<J = ()> {
    judge: J,
    timeout: Duration,
    meter: Option<Meter>,
    name: Option<&'static str>,
}
impl Controller<()> {
    pub fn builder() -> ControllerBuilder {
        ControllerBuilder {
            judge: (),
            timeout: Duration::from_secs(2),
            meter: None,
            name: None,
        }
    }
}
impl<J> ControllerBuilder<J> {
    pub fn judge<N>(self, judge: N) -> ControllerBuilder<N> {
        ControllerBuilder {
            judge,
            timeout: self.timeout,
            meter: self.meter,
            name: self.name,
        }
    }
    /// Optional stable telemetry label. Avoid request IDs or dynamically generated names.
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }
    /// Application-owned OpenTelemetry meter; defaults to the global provider at build time.
    pub fn meter(mut self, meter: Meter) -> Self {
        self.meter = Some(meter);
        self
    }
    pub fn inference_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn build(self) -> Result<Controller<J>, ControllerConfigError> {
        if self.timeout.is_zero() {
            return Err(ControllerConfigError);
        }
        Ok(Controller {
            judge: self.judge,
            timeout: self.timeout,
            telemetry: Telemetry::new(self.meter, self.name, true),
        })
    }
}
impl<J> Controller<J> {
    /// Deadline covers the entire judge operation, including provider retries.
    pub async fn evaluate<S, A>(&self, state: &S) -> Result<ProposedDecision<A>, EvaluationError>
    where
        J: Judge<S, A>,
    {
        let mut operation = self.telemetry.evaluation();
        let span = operation.span.clone();
        scope(
            async {
                let result = async {
                    let judgment = tokio::time::timeout(self.timeout, self.judge.judge(state))
                        .await
                        .map_err(|_| EvaluationError::Timeout)??;
                    judgment.try_into()
                }
                .await;
                operation.finish_evaluation(&result);
                result
            }
            .instrument(span),
        )
        .await
    }
    pub fn judge(&self) -> &J {
        &self.judge
    }
}
