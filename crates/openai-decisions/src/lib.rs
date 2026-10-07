// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Standalone typed OpenAI Decisions client. No dependency on Reflex.
mod client;
mod questions;
mod telemetry;
pub use client::*;
pub use questions::*;
#[doc(hidden)]
pub mod __private {
    pub use serde_json;
    use serde_json::Value;
    use std::collections::BTreeMap;

    /// Add the question's name to its wire object.
    pub fn named(name: &str, question: Value) -> Result<Value, crate::Error> {
        let Value::Object(mut map) = question else {
            return Err(crate::Error::configuration("question must be an object"));
        };
        map.insert("name".into(), name.into());
        Ok(map.into())
    }
    /// Index the wire answers by name, requiring exactly the expected names.
    /// The first refused question fails the whole call.
    pub fn answers<'a>(
        value: &'a Value,
        expected: &[&str],
    ) -> Result<BTreeMap<&'a str, &'a Value>, crate::Error> {
        let list = value
            .as_array()
            .ok_or_else(|| crate::Error::invalid_response("answers must be an array"))?;
        let mut answers = BTreeMap::new();
        for answer in list {
            let name = answer.get("name").and_then(Value::as_str);
            let Some(name) = name.filter(|n| expected.contains(n)) else {
                return Err(crate::Error::invalid_response(
                    "answer names do not match questions",
                ));
            };
            if answers.insert(name, answer).is_some() {
                return Err(crate::Error::invalid_response(
                    "answer names do not match questions",
                ));
            }
        }
        if answers.len() != expected.len() {
            return Err(crate::Error::invalid_response(
                "answer names do not match questions",
            ));
        }
        for name in expected {
            if answers[name].get("type").and_then(Value::as_str) == Some("refusal") {
                return Err(crate::Error::Refusal {
                    question: (*name).to_owned(),
                });
            }
        }
        Ok(answers)
    }
}

/// Build a heterogeneous set of named questions with named, typed answers.
///
/// ```
/// use openai_decisions::{choice, predicate, questions, DecisionTask};
/// let task = DecisionTask::builder().model("gpt-6-luna").questions(questions! {
///     action: choice("Choose an action", [("admit", "Accept"), ("defer", "Wait")]),
///     urgent: predicate("Is this urgent?"),
/// }).build().unwrap();
/// ```
#[macro_export]
macro_rules! questions {
    ($($name:ident: $question:expr),+ $(,)?) => {{
        #[allow(non_camel_case_types)]
        #[derive(Clone)]
        struct Questions<$($name),+> { $($name: $name),+ }
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone)]
        struct Answers<$($name),+> { $(pub $name: $name),+ }
        #[allow(non_camel_case_types)]
        impl<$($name: $crate::Question),+> $crate::QuestionSet for Questions<$($name),+> {
            type Answers = Answers<$($name::Answer),+>;
            fn encode(&self) -> Result<$crate::__private::serde_json::Value, $crate::Error> {
                Ok($crate::__private::serde_json::Value::Array(vec![
                    $($crate::__private::named(stringify!($name), self.$name.encode()?)?),+
                ]))
            }
            fn decode(&self, value: &$crate::__private::serde_json::Value) -> Result<Self::Answers, $crate::Error> {
                let answers = $crate::__private::answers(value, &[$(stringify!($name)),+])?;
                Ok(Answers { $($name: self.$name.decode(answers[stringify!($name)])?),+ })
            }
        }
        Questions { $($name: $question),+ }
    }};
}
