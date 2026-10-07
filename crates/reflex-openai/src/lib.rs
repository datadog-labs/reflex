// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Inject an instantiated OpenAI client and typed task into Reflex.
use openai_decisions::{ChoiceAnswer, DecisionTask, DecisionsClient, QuestionSet, Usage};
use reflex::{Judge, JudgeError, Judgment};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone)]
pub struct JudgeDiagnostics {
    pub model: String,
    pub usage: Option<Usage>,
    pub request_id: Option<String>,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: Option<f64>,
}
/// Diagnostics retain the latest successfully decoded response. Concurrent
/// evaluations can replace this slot; it is not a per-decision audit trail.
#[derive(Clone)]
pub struct DecisionsJudge<Q, F = ()> {
    client: DecisionsClient,
    task: DecisionTask<Q>,
    select: F,
    last_response: Arc<Mutex<Option<JudgeDiagnostics>>>,
}
impl<Q> DecisionsJudge<Q> {
    pub fn new(client: DecisionsClient, task: DecisionTask<Q>) -> Self {
        Self {
            client,
            task,
            select: (),
            last_response: Arc::new(Mutex::new(None)),
        }
    }
    pub fn select_answer<F, A>(self, select: F) -> DecisionsJudge<Q, F>
    where
        Q: QuestionSet,
        F: Fn(Q::Answers) -> ChoiceAnswer<A> + Send + Sync,
    {
        DecisionsJudge {
            client: self.client,
            task: self.task,
            select,
            last_response: self.last_response,
        }
    }
}
impl<Q, F> DecisionsJudge<Q, F> {
    pub fn last_response(&self) -> Option<JudgeDiagnostics> {
        self.last_response
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
impl<S, A, Q, F> Judge<S, A> for DecisionsJudge<Q, F>
where
    S: Serialize + Sync,
    A: Send,
    Q: QuestionSet,
    F: Fn(Q::Answers) -> ChoiceAnswer<A> + Send + Sync,
{
    async fn judge(&self, state: &S) -> Result<Judgment<A>, JudgeError> {
        let response = self.client.decide(&self.task, state).await.map_err(|e| {
            let code = match &e {
                openai_decisions::Error::Http { status, .. } => format!("openai_http_{status}"),
                _ => format!("openai_{}", e.code()),
            };
            JudgeError::new(code, e.to_string())
        })?;
        let answer = (self.select)(response.answers);
        *self.last_response.lock().unwrap_or_else(|e| e.into_inner()) = Some(JudgeDiagnostics {
            model: response.model,
            usage: response.usage,
            request_id: response.request_id,
            probabilities: answer.probabilities,
            confidence: answer.confidence,
        });
        Ok(Judgment {
            action: answer.choice,
            confidence: answer.confidence,
        })
    }
}
