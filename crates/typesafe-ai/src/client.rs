use crate::{
    telemetry::{Call, Telemetry},
    QuestionSet,
};
use opentelemetry::metrics::Meter;
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client, Url,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use thiserror::Error;
use tracing::Instrument;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid client or task configuration: {0}")]
    Configuration(String),
    #[error("TypeSafe operation deadline exceeded")]
    Timeout,
    #[error("TypeSafe HTTP {status}")]
    Http {
        status: u16,
        request_id: Option<String>,
    },
    #[error("TypeSafe transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("invalid TypeSafe response: {0}")]
    InvalidResponse(String),
    #[error("JSON encoding/decoding failed: {0}")]
    Json(#[from] serde_json::Error),
}
impl Error {
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }
    pub fn invalid_response(message: impl Into<String>) -> Self {
        Self::InvalidResponse(message.into())
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Timeout => "timeout",
            Self::Http { .. } => "http",
            Self::Transport(_) => "transport",
            Self::InvalidResponse(_) => "invalid_response",
            Self::Json(_) => "json",
        }
    }
}

#[derive(Clone)]
pub struct SystemOneTask<Q> {
    model: String,
    questions: Q,
    wire_questions: Value,
}
pub struct TaskBuilder<Q = ()> {
    model: Option<String>,
    questions: Q,
}
impl SystemOneTask<()> {
    pub fn builder() -> TaskBuilder {
        TaskBuilder {
            model: None,
            questions: (),
        }
    }
}
impl<Q> TaskBuilder<Q> {
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
    pub fn questions<N>(self, questions: N) -> TaskBuilder<N> {
        TaskBuilder {
            model: self.model,
            questions,
        }
    }
}
impl<Q: QuestionSet> TaskBuilder<Q> {
    pub fn build(self) -> Result<SystemOneTask<Q>, Error> {
        let model = self
            .model
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::configuration("model is required"))?;
        let wire_questions = self.questions.encode()?;
        let map = wire_questions
            .as_object()
            .ok_or_else(|| Error::configuration("questions must be an object"))?;
        if map.is_empty() || map.keys().any(|s| s.is_empty()) {
            return Err(Error::configuration("named questions are required"));
        }
        Ok(SystemOneTask {
            model,
            questions: self.questions,
            wire_questions,
        })
    }
}
impl<Q> SystemOneTask<Q> {
    pub fn model(&self) -> &str {
        &self.model
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}
#[derive(Debug, Clone)]
pub struct Response<A> {
    pub answers: A,
    /// Actual provider model, which may resolve a requested alias.
    pub model: String,
    pub usage: Usage,
    pub request_id: Option<String>,
}
/// A reusable connection pool. Authentication is never exposed through Debug.
#[derive(Clone)]
pub struct TypeSafeClient {
    http: Client,
    endpoint: Url,
    timeout: Duration,
    max_retries: u32,
    telemetry: Telemetry,
}
pub struct ClientBuilder {
    api_key: Option<String>,
    endpoint: String,
    timeout: Duration,
    max_retries: u32,
    meter: Option<Meter>,
}
impl TypeSafeClient {
    pub fn builder() -> ClientBuilder {
        ClientBuilder {
            api_key: None,
            endpoint: "https://api.typesafe.ai/v1/systemone".into(),
            timeout: Duration::from_secs(10),
            max_retries: 2,
            meter: None,
        }
    }
    /// Overall deadline includes connection, response body, and bounded backoff.
    pub async fn system_one<Q: QuestionSet, S: Serialize + ?Sized>(
        &self,
        task: &SystemOneTask<Q>,
        state: &S,
    ) -> Result<Response<Q::Answers>, Error> {
        let mut call = self
            .telemetry
            .start(task.model(), self.timeout, self.max_retries);
        let span = call.span.clone();
        async {
            let result = async {
                let state = serde_json::to_value(state)?;
                if !matches!(state, Value::String(_) | Value::Array(_) | Value::Object(_)) {
                    return Err(Error::configuration(
                        "state must serialize as a string, array, or object",
                    ));
                }
                let request =
                    json!({"model":task.model,"state":state,"questions":task.wire_questions});
                tokio::time::timeout(self.timeout, self.send(task, &request, &mut call))
                    .await
                    .map_err(|_| Error::Timeout)?
            }
            .await;
            call.finish(&result);
            result
        }
        .instrument(span)
        .await
    }
    async fn send<Q: QuestionSet>(
        &self,
        task: &SystemOneTask<Q>,
        request: &Value,
        call: &mut Call,
    ) -> Result<Response<Q::Answers>, Error> {
        for attempt in 0..=self.max_retries {
            let attempt_span = call.begin_attempt();
            let mut response = self
                .http
                .post(self.endpoint.clone())
                .json(request)
                .send()
                .instrument(attempt_span.clone())
                .await?;
            let status = response.status().as_u16();
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            call.response(status, request_id.as_deref());
            if !response.status().is_success() {
                if attempt < self.max_retries && [429, 500, 502, 503, 504, 529].contains(&status) {
                    let delay = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .and_then(|v| Duration::try_from_secs_f64(v).ok())
                        .unwrap_or_else(|| Duration::from_millis(100 * (1 << attempt)));
                    call.end_attempt("http_error", Some("http"));
                    // Close this attempt's span before waiting for the next one.
                    drop(attempt_span);
                    call.begin_backoff(delay, status);
                    tokio::time::sleep(delay).await;
                    call.end_backoff("completed");
                    continue;
                }
                call.retries_exhausted = Some(
                    attempt == self.max_retries && [429, 500, 502, 503, 504, 529].contains(&status),
                );
                return Err(Error::Http { status, request_id });
            }
            call.stage("reading_response");
            const MAX_BODY: usize = 8 * 1024 * 1024;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().instrument(attempt_span.clone()).await? {
                if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
                    return Err(Error::invalid_response("response exceeds 8 MiB"));
                }
                bytes.extend_from_slice(&chunk);
            }
            call.stage("validating_response");
            let raw: Value = serde_json::from_slice(&bytes)
                .map_err(|_| Error::invalid_response("response is not valid JSON"))?;
            let model = raw
                .get("model")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::invalid_response("missing model"))?
                .to_owned();
            let usage: Usage = serde_json::from_value(
                raw.get("usage")
                    .cloned()
                    .ok_or_else(|| Error::invalid_response("missing usage"))?,
            )
            .map_err(|_| Error::invalid_response("invalid usage"))?;
            // Count valid reported usage even when typed answer validation later fails.
            call.usage(&model, usage.input_tokens, usage.output_tokens);
            let answers = task.questions.decode(
                raw.get("answers")
                    .ok_or_else(|| Error::invalid_response("missing answers"))?,
            )?;
            return Ok(Response {
                answers,
                model,
                usage,
                request_id,
            });
        }
        unreachable!("bounded retry loop always returns")
    }
}
impl ClientBuilder {
    /// Use an application-owned OpenTelemetry meter. Otherwise the global provider is
    /// consulted at build time; initialize it before building clients.
    pub fn meter(mut self, meter: Meter) -> Self {
        self.meter = Some(meter);
        self
    }
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
    /// Complete endpoint URL. HTTPS is required except for loopback test servers.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }
    pub fn build(self) -> Result<TypeSafeClient, Error> {
        let key = self
            .api_key
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::configuration("api_key is required"))?;
        if self.timeout.is_zero() || self.max_retries > 10 {
            return Err(Error::configuration(
                "timeout must be positive and max_retries at most ten",
            ));
        }
        let endpoint =
            Url::parse(&self.endpoint).map_err(|_| Error::configuration("invalid endpoint URL"))?;
        let loopback = endpoint.host_str().is_some_and(|h| {
            h == "localhost"
                || h == "[::1]"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && loopback)
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(Error::configuration(
                "endpoint requires HTTPS (or loopback HTTP), no user info, and no fragment",
            ));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", key.trim()))
            .map_err(|_| Error::configuration("invalid api_key header"))?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(TypeSafeClient {
            http,
            endpoint,
            timeout: self.timeout,
            max_retries: self.max_retries,
            telemetry: Telemetry::new(
                self.meter
                    .unwrap_or_else(|| opentelemetry::global::meter("typesafe-ai")),
            ),
        })
    }
}
