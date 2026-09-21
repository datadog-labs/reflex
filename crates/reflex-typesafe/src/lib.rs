//! Inject an instantiated TypeSafe client and typed task into Reflex.
use reflex::{Judge, JudgeError, Judgment};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use typesafe_ai::{ChoiceAnswer, QuestionSet, SystemOneTask, TypeSafeClient, Usage};

#[derive(Debug, Clone)]
pub struct JudgeDiagnostics {
    pub model: String,
    pub usage: Usage,
    pub request_id: Option<String>,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: Option<f64>,
}
/// Diagnostics retain the latest successfully decoded response. Concurrent
/// evaluations can replace this slot; it is not a per-decision audit trail.
#[derive(Clone)]
pub struct TypeSafeJudge<Q, F = ()> {
    client: TypeSafeClient,
    task: SystemOneTask<Q>,
    select: F,
    last_response: Arc<Mutex<Option<JudgeDiagnostics>>>,
}
impl<Q> TypeSafeJudge<Q> {
    pub fn new(client: TypeSafeClient, task: SystemOneTask<Q>) -> Self {
        Self {
            client,
            task,
            select: (),
            last_response: Arc::new(Mutex::new(None)),
        }
    }
    pub fn select_answer<F, A>(self, select: F) -> TypeSafeJudge<Q, F>
    where
        Q: QuestionSet,
        F: Fn(Q::Answers) -> ChoiceAnswer<A> + Send + Sync,
    {
        TypeSafeJudge {
            client: self.client,
            task: self.task,
            select,
            last_response: self.last_response,
        }
    }
}
impl<Q, F> TypeSafeJudge<Q, F> {
    pub fn last_response(&self) -> Option<JudgeDiagnostics> {
        self.last_response
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
impl<S, A, Q, F> Judge<S, A> for TypeSafeJudge<Q, F>
where
    S: Serialize + Sync,
    A: Send,
    Q: QuestionSet,
    F: Fn(Q::Answers) -> ChoiceAnswer<A> + Send + Sync,
{
    async fn judge(&self, state: &S) -> Result<Judgment<A>, JudgeError> {
        let response = self
            .client
            .system_one(&self.task, state)
            .await
            .map_err(|e| {
                let code = match &e {
                    typesafe_ai::Error::Http { status, .. } => format!("typesafe_http_{status}"),
                    _ => format!("typesafe_{}", e.code()),
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
