// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub trait Question: Send + Sync {
    type Answer: Send;
    fn encode(&self) -> Result<Value, Error>;
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error>;
}
pub trait QuestionSet: Send + Sync {
    type Answers: Send;
    fn encode(&self) -> Result<Value, Error>;
    fn decode(&self, value: &Value) -> Result<Self::Answers, Error>;
}
#[derive(Debug, Clone)]
pub struct Choice<A> {
    instructions: Value,
    options: Vec<(A, String)>,
}
/// Option values serialize to wire labels; string values use their string,
/// other values use canonical serde_json text. Duplicate labels are rejected.
pub fn choice<A>(
    instructions: impl Into<Value>,
    options: impl IntoIterator<Item = (A, impl Into<String>)>,
) -> Choice<A> {
    Choice {
        instructions: instructions.into(),
        options: options.into_iter().map(|(a, s)| (a, s.into())).collect(),
    }
}
#[derive(Debug, Clone)]
pub struct ChoiceAnswer<A> {
    pub choice: A,
    pub confidence: Option<f64>,
    /// Original provider labels and distribution are retained without rounding.
    pub probabilities: BTreeMap<String, f64>,
}
fn label<A: Serialize>(value: &A) -> Result<String, Error> {
    let value = serde_json::to_value(value)?;
    Ok(match value {
        Value::String(s) => s,
        other => other.to_string(),
    })
}
fn instructions(value: &Value) -> Result<(), Error> {
    if !matches!(value, Value::String(_) | Value::Array(_) | Value::Object(_)) {
        return Err(Error::configuration(
            "instructions must be a string, object, or array",
        ));
    }
    Ok(())
}
fn typed(value: &Value, expected: &str) -> Result<(), Error> {
    if value.get("type").and_then(Value::as_str) != Some(expected) {
        return Err(Error::invalid_response(
            "answer type does not match question",
        ));
    }
    Ok(())
}
fn unit(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn confidence(value: &Value) -> Result<Option<f64>, Error> {
    match value.get("confidence") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|v| unit(*v))
            .map(Some)
            .ok_or_else(|| Error::invalid_response("invalid confidence")),
    }
}
fn distribution(value: &Value, labels: &[String]) -> Result<BTreeMap<String, f64>, Error> {
    let probabilities: BTreeMap<String, f64> = serde_json::from_value(
        value
            .get("probabilities")
            .cloned()
            .ok_or_else(|| Error::invalid_response("missing probabilities"))?,
    )
    .map_err(|_| Error::invalid_response("invalid probabilities"))?;
    if probabilities.len() != labels.len()
        || labels.iter().any(|s| !probabilities.contains_key(s))
        || probabilities.values().any(|v| !unit(*v))
        || (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
    {
        return Err(Error::invalid_response(
            "probabilities must cover exactly the options and sum to one",
        ));
    }
    Ok(probabilities)
}
impl<A: Clone + Serialize + Send + Sync> Question for Choice<A> {
    type Answer = ChoiceAnswer<A>;
    fn encode(&self) -> Result<Value, Error> {
        instructions(&self.instructions)?;
        if self.options.len() < 2 {
            return Err(Error::configuration("choice requires at least two options"));
        }
        let mut criteria = BTreeMap::new();
        for (value, description) in &self.options {
            let key = label(value)?;
            if key.is_empty() || criteria.insert(key, description).is_some() {
                return Err(Error::configuration(
                    "choice labels must be nonempty and unique",
                ));
            }
        }
        Ok(json!({"type":"choice","instructions":self.instructions,"criteria":criteria}))
    }
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error> {
        typed(value, "choice")?;
        let labels = self
            .options
            .iter()
            .map(|(a, _)| label(a))
            .collect::<Result<Vec<_>, _>>()?;
        let probabilities = distribution(value, &labels)?;
        let selected = value
            .get("choice")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid_response("missing choice"))?;
        let index = labels
            .iter()
            .position(|s| s == selected)
            .ok_or_else(|| Error::invalid_response("unknown choice label"))?;
        if probabilities
            .values()
            .any(|p| *p > probabilities[selected] + 0.001)
        {
            return Err(Error::invalid_response(
                "selected choice is not a highest-probability option",
            ));
        }
        Ok(ChoiceAnswer {
            choice: self.options[index].0.clone(),
            confidence: confidence(value)?,
            probabilities,
        })
    }
}
#[derive(Debug, Clone)]
pub struct Noul {
    instructions: Value,
}
pub fn noul(instructions: impl Into<Value>) -> Noul {
    Noul {
        instructions: instructions.into(),
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoulAnswer {
    pub noul: f64,
}
impl Question for Noul {
    type Answer = NoulAnswer;
    fn encode(&self) -> Result<Value, Error> {
        instructions(&self.instructions)?;
        Ok(json!({"type":"noul","instructions":self.instructions}))
    }
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error> {
        typed(value, "noul")?;
        let noul = value
            .get("noul")
            .and_then(Value::as_f64)
            .filter(|v| unit(*v))
            .ok_or_else(|| Error::invalid_response("noul must be in 0..=1"))?;
        Ok(NoulAnswer { noul })
    }
}
#[derive(Debug, Clone)]
pub struct Score {
    instructions: Value,
    levels: Vec<String>,
}
pub fn score(
    instructions: impl Into<Value>,
    levels: impl IntoIterator<Item = impl Into<String>>,
) -> Score {
    Score {
        instructions: instructions.into(),
        levels: levels.into_iter().map(Into::into).collect(),
    }
}
#[derive(Debug, Clone)]
pub struct ScoreAnswer {
    pub score: f64,
    pub legend: BTreeMap<String, String>,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: Option<f64>,
}
impl Question for Score {
    type Answer = ScoreAnswer;
    fn encode(&self) -> Result<Value, Error> {
        instructions(&self.instructions)?;
        if self.levels.len() < 2 {
            return Err(Error::configuration("score requires at least two levels"));
        }
        Ok(json!({"type":"score","instructions":self.instructions,"criteria":self.levels}))
    }
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error> {
        typed(value, "score")?;
        let legend: BTreeMap<String, String> = self
            .levels
            .iter()
            .enumerate()
            .map(|(i, s)| (i.to_string(), s.clone()))
            .collect();
        let actual: BTreeMap<String, String> = serde_json::from_value(
            value
                .get("legend")
                .cloned()
                .ok_or_else(|| Error::invalid_response("missing score legend"))?,
        )
        .map_err(|_| Error::invalid_response("invalid score legend"))?;
        if actual != legend {
            return Err(Error::invalid_response(
                "score legend does not match requested levels",
            ));
        }
        let probabilities = distribution(value, &legend.keys().cloned().collect::<Vec<_>>())?;
        let score = value
            .get("score")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= (self.levels.len() - 1) as f64)
            .ok_or_else(|| Error::invalid_response("score is outside the rubric"))?;
        Ok(ScoreAnswer {
            score,
            legend,
            probabilities,
            confidence: confidence(value)?,
        })
    }
}
