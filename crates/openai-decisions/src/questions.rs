// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub trait Question: Send + Sync {
    type Answer: Send;
    /// The wire question without its `name`, which the question set supplies.
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
    instructions: String,
    options: Vec<(A, String)>,
}
/// Option values serialize to wire values; string values use their string,
/// other values use canonical serde_json text. Duplicate values are rejected.
pub fn choice<A>(
    instructions: impl Into<String>,
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
    /// Original provider values and distribution are retained without rounding.
    pub probabilities: BTreeMap<String, f64>,
}
fn label<A: Serialize>(value: &A) -> Result<String, Error> {
    let value = serde_json::to_value(value)?;
    Ok(match value {
        Value::String(s) => s,
        other => other.to_string(),
    })
}
fn instructions(value: &str) -> Result<(), Error> {
    if value.trim().is_empty() {
        return Err(Error::configuration("instructions are required"));
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
/// The wire distribution is an array with one entry per requested option, in any order.
fn distribution(value: &Value, count: usize) -> Result<&Vec<Value>, Error> {
    let entries = value
        .get("probabilities")
        .ok_or_else(|| Error::invalid_response("missing probabilities"))?
        .as_array()
        .ok_or_else(|| Error::invalid_response("invalid probabilities"))?;
    let probabilities = entries
        .iter()
        .map(|e| e.get("probability").and_then(Value::as_f64))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Error::invalid_response("invalid probabilities"))?;
    if probabilities.len() != count
        || probabilities.iter().any(|v| !unit(*v))
        || (probabilities.iter().sum::<f64>() - 1.0).abs() > 0.01
    {
        return Err(Error::invalid_response(
            "probabilities must cover exactly the options and sum to one",
        ));
    }
    Ok(entries)
}
impl<A: Clone + Serialize + Send + Sync> Question for Choice<A> {
    type Answer = ChoiceAnswer<A>;
    fn encode(&self) -> Result<Value, Error> {
        instructions(&self.instructions)?;
        if self.options.len() < 2 {
            return Err(Error::configuration("choice requires at least two options"));
        }
        let mut choices = Vec::new();
        for (value, description) in &self.options {
            let value = label(value)?;
            if value.is_empty() || choices.iter().any(|c: &Value| c["value"] == value) {
                return Err(Error::configuration(
                    "choice values must be nonempty and unique",
                ));
            }
            choices.push(json!({"value":value,"description":description}));
        }
        Ok(json!({"type":"choice","instructions":self.instructions,"choices":choices}))
    }
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error> {
        typed(value, "choice")?;
        let labels = self
            .options
            .iter()
            .map(|(a, _)| label(a))
            .collect::<Result<Vec<_>, _>>()?;
        let mut probabilities = BTreeMap::new();
        for entry in distribution(value, labels.len())? {
            let key = entry.get("value").and_then(Value::as_str);
            let Some(key) = key.filter(|k| labels.iter().any(|s| s == k)) else {
                return Err(Error::invalid_response(
                    "probabilities must cover exactly the options and sum to one",
                ));
            };
            probabilities.insert(key.to_owned(), entry["probability"].as_f64().unwrap_or(0.0));
        }
        // Duplicate entries collapse, leaving a requested option uncovered.
        if probabilities.len() != labels.len() {
            return Err(Error::invalid_response(
                "probabilities must cover exactly the options and sum to one",
            ));
        }
        let selected = value
            .get("choice")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid_response("missing choice"))?;
        let index = labels
            .iter()
            .position(|s| s == selected)
            .ok_or_else(|| Error::invalid_response("unknown choice value"))?;
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
pub struct Predicate {
    instructions: String,
}
pub fn predicate(instructions: impl Into<String>) -> Predicate {
    Predicate {
        instructions: instructions.into(),
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PredicateAnswer {
    /// Probability that the predicate holds.
    pub probability: f64,
}
impl Question for Predicate {
    type Answer = PredicateAnswer;
    fn encode(&self) -> Result<Value, Error> {
        instructions(&self.instructions)?;
        Ok(json!({"type":"predicate","instructions":self.instructions}))
    }
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error> {
        typed(value, "predicate")?;
        let probability = value
            .get("probability")
            .and_then(Value::as_f64)
            .filter(|v| unit(*v))
            .ok_or_else(|| Error::invalid_response("probability must be in 0..=1"))?;
        Ok(PredicateAnswer { probability })
    }
}
#[derive(Debug, Clone)]
pub struct Score {
    instructions: String,
    levels: Vec<(String, String)>,
}
/// Levels are `(label, description)` pairs ordered from lowest to highest.
pub fn score(
    instructions: impl Into<String>,
    levels: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> Score {
    Score {
        instructions: instructions.into(),
        levels: levels
            .into_iter()
            .map(|(l, d)| (l.into(), d.into()))
            .collect(),
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct LevelProbability {
    pub label: String,
    pub probability: f64,
}
#[derive(Debug, Clone)]
pub struct ScoreAnswer {
    /// Probability-weighted average of level positions, starting at zero.
    pub score: f64,
    /// One entry per requested level, in rubric order.
    pub probabilities: Vec<LevelProbability>,
    pub confidence: Option<f64>,
}
impl Question for Score {
    type Answer = ScoreAnswer;
    fn encode(&self) -> Result<Value, Error> {
        instructions(&self.instructions)?;
        if self.levels.len() < 2 {
            return Err(Error::configuration("score requires at least two levels"));
        }
        let mut levels = Vec::new();
        for (label, description) in &self.levels {
            if label.is_empty() || levels.iter().any(|l: &Value| l["label"] == *label) {
                return Err(Error::configuration(
                    "score labels must be nonempty and unique",
                ));
            }
            levels.push(json!({"label":label,"description":description}));
        }
        Ok(json!({"type":"score","instructions":self.instructions,"levels":levels}))
    }
    fn decode(&self, value: &Value) -> Result<Self::Answer, Error> {
        typed(value, "score")?;
        let mut probabilities = vec![None; self.levels.len()];
        for entry in distribution(value, self.levels.len())? {
            let slot = entry
                .get("value")
                .and_then(Value::as_u64)
                .and_then(|i| usize::try_from(i).ok())
                .filter(|i| {
                    self.levels.get(*i).map(|(l, _)| l.as_str())
                        == entry.get("label").and_then(Value::as_str)
                })
                .and_then(|i| probabilities.get_mut(i))
                .filter(|slot| slot.is_none())
                .ok_or_else(|| {
                    Error::invalid_response("score levels do not match requested levels")
                })?;
            *slot = entry["probability"].as_f64();
        }
        let probabilities = self
            .levels
            .iter()
            .zip(probabilities)
            .map(|((label, _), probability)| {
                Some(LevelProbability {
                    label: label.clone(),
                    probability: probability?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::invalid_response("score levels do not match requested levels"))?;
        let score = value
            .get("score")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= (self.levels.len() - 1) as f64)
            .ok_or_else(|| Error::invalid_response("score is outside the rubric"))?;
        Ok(ScoreAnswer {
            score,
            probabilities,
            confidence: confidence(value)?,
        })
    }
}
