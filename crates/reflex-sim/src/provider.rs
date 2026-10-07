// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! The model provider behind the simulator's Jev policy: TypeSafe's Jev or OpenAI Decisions.
use openai_decisions::DecisionsClient;
use reflex::{Judge, JudgeError, Judgment};
use reflex_openai::DecisionsJudge;
use reflex_typesafe::TypeSafeJudge;
use std::{collections::BTreeMap, time::Duration};
use typesafe_ai::{TypeSafeClient, Usage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Provider {
    #[default]
    Typesafe,
    Openai,
}
impl Provider {
    /// Environment variable holding this provider's API key.
    pub fn key_variable(self) -> &'static str {
        match self {
            Self::Typesafe => "TYPESAFE_API_KEY",
            Self::Openai => "OPENAI_API_KEY",
        }
    }
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Typesafe => "jev-1.13.0",
            Self::Openai => "gpt-6-luna",
        }
    }
    /// A client with the playground's settings: a two-second deadline and no retries.
    /// `name` labels the client's request metrics.
    pub fn client(self, key: &str, name: &'static str) -> Result<ModelClient, String> {
        let timeout = Duration::from_secs(2);
        match self {
            Self::Typesafe => TypeSafeClient::builder()
                .api_key(key)
                .name(name)
                .meter(opentelemetry::global::meter("typesafe-ai"))
                .timeout(timeout)
                .max_retries(0)
                .build()
                .map(ModelClient::TypeSafe)
                .map_err(|e| e.to_string()),
            Self::Openai => DecisionsClient::builder()
                .api_key(key)
                .name(name)
                .meter(opentelemetry::global::meter("openai-decisions"))
                .timeout(timeout)
                .max_retries(0)
                .build()
                .map(ModelClient::OpenAi)
                .map_err(|e| e.to_string()),
        }
    }
}
/// Published input price in USD per million tokens for a resolved model; output is
/// free for both. `None` when the model has no known price.
///
/// Jev 1.13: <https://docs.typesafe.ai/models>, verified 2026-09-20.
/// GPT-6 Luna: <https://developers.openai.com/api/docs/guides/decisions>, verified
/// 2026-10-06; excludes regional-processing premiums and long-context multipliers.
pub fn input_usd_per_million(model: &str) -> Option<f64> {
    match model {
        "jev-1.13.0" => Some(0.042),
        "gpt-6-luna" => Some(0.10),
        _ => None,
    }
}
#[derive(Clone)]
pub enum ModelClient {
    TypeSafe(TypeSafeClient),
    OpenAi(DecisionsClient),
}
impl From<TypeSafeClient> for ModelClient {
    fn from(client: TypeSafeClient) -> Self {
        Self::TypeSafe(client)
    }
}
impl From<DecisionsClient> for ModelClient {
    fn from(client: DecisionsClient) -> Self {
        Self::OpenAi(client)
    }
}
/// What either provider reports about its latest decoded response.
#[derive(Debug, Clone)]
pub struct Diagnostics {
    pub model: String,
    /// Missing when the provider reported none; never assumed zero.
    pub usage: Option<Usage>,
    pub request_id: Option<String>,
    pub probabilities: BTreeMap<String, f64>,
}
/// One provider's judge, chosen at run time.
pub enum ModelJudge<T, O> {
    TypeSafe(T),
    OpenAi(O),
}
impl<S: Sync, A, T: Judge<S, A>, O: Judge<S, A>> Judge<S, A> for ModelJudge<T, O> {
    async fn judge(&self, state: &S) -> Result<Judgment<A>, JudgeError> {
        match self {
            Self::TypeSafe(judge) => judge.judge(state).await,
            Self::OpenAi(judge) => judge.judge(state).await,
        }
    }
}
impl<Q, F, P, G> ModelJudge<TypeSafeJudge<Q, F>, DecisionsJudge<P, G>> {
    pub fn last_response(&self) -> Option<Diagnostics> {
        match self {
            Self::TypeSafe(judge) => judge.last_response().map(|d| Diagnostics {
                model: d.model,
                usage: Some(d.usage),
                request_id: d.request_id,
                probabilities: d.probabilities,
            }),
            Self::OpenAi(judge) => judge.last_response().map(|d| Diagnostics {
                model: d.model,
                usage: d.usage.map(|u| Usage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    extra: u.extra,
                }),
                request_id: d.request_id,
                probabilities: d.probabilities,
            }),
        }
    }
}
/// Build a judge asking one choice question of the configured provider. The question
/// has the same name, instructions and options whichever provider answers it.
macro_rules! choice_judge {
    ($client:expr, $model:expr, $name:ident: $instructions:expr, $options:expr) => {
        match $client {
            $crate::provider::ModelClient::TypeSafe(client) => {
                typesafe_ai::SystemOneTask::builder()
                    .model($model)
                    .questions(typesafe_ai::questions! {
                        $name: typesafe_ai::choice($instructions, $options)
                    })
                    .build()
                    .map(|task| {
                        $crate::provider::ModelJudge::TypeSafe(
                            reflex_typesafe::TypeSafeJudge::new(client.clone(), task)
                                .select_answer(|a| a.$name),
                        )
                    })
                    .map_err(|e| e.to_string())
            }
            $crate::provider::ModelClient::OpenAi(client) => {
                openai_decisions::DecisionTask::builder()
                    .model($model)
                    .questions(openai_decisions::questions! {
                        $name: openai_decisions::choice($instructions, $options)
                    })
                    .build()
                    .map(|task| {
                        $crate::provider::ModelJudge::OpenAi(
                            reflex_openai::DecisionsJudge::new(client.clone(), task)
                                .select_answer(|a| a.$name),
                        )
                    })
                    .map_err(|e| e.to_string())
            }
        }
    };
}
pub(crate) use choice_judge;
