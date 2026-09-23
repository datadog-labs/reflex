// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::forecast::{Input, CONTEXT, EPOCH, HORIZON};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Cycles,
    Ramp,
    Surprise,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub id: usize,
    pub rate: f64,
    pub cpu: u32,
    pub memory_gib: u32,
    pub duration_s: u64,
    pub enabled: bool,
}
impl Client {
    pub fn valid(&self) -> bool {
        self.id < 8
            && self.rate.is_finite()
            && (0. ..=4.).contains(&self.rate)
            && (1..=16).contains(&self.cpu)
            && (1..=32).contains(&self.memory_gib)
            && (1..=30).contains(&self.duration_s)
    }
}
pub fn defaults() -> Vec<Client> {
    [(0.35, 1, 2, 8), (0.20, 2, 4, 10), (0.10, 4, 8, 12)]
        .into_iter()
        .enumerate()
        .map(|(id, (rate, cpu, memory_gib, duration_s))| Client {
            id,
            rate,
            cpu,
            memory_gib,
            duration_s,
            enabled: true,
        })
        .collect()
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Arrival {
    pub id: u64,
    pub client: usize,
    pub cpu: u32,
    pub memory_gib: u32,
    pub estimated_s: u64,
    pub actual_s: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    pub at_ms: i64,
    pub values: [f32; 3],
    pub jobs: Vec<Arrival>,
}
fn draw(seed: u64, t: i64, c: usize, salt: u64) -> f64 {
    let mut v =
        seed ^ (t as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ (c as u64).wrapping_mul(137) ^ salt;
    v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((v ^ (v >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}
pub fn multiplier(s: Scenario, t: i64) -> f64 {
    match s {
        Scenario::Cycles => 1.8 + 1.3 * (t as f64 / 150. * std::f64::consts::TAU).sin(),
        Scenario::Ramp => {
            if t < 0 {
                1.
            } else if t < 200 {
                1. + t as f64 / 100.
            } else if t < 350 {
                3.
            } else {
                (3. - (t - 350) as f64 / 100.).max(0.7)
            }
        }
        Scenario::Surprise => {
            if (150..260).contains(&t) {
                3.5
            } else {
                0.9
            }
        }
    }
}
pub fn bucket(seed: u64, s: Scenario, clients: &[Client], t: i64) -> Bucket {
    let mut jobs = vec![];
    let mut values = [0f32; 3];
    for c in clients.iter().filter(|c| c.enabled) {
        let offered = c.rate * multiplier(s, t);
        let n = offered.floor() as usize + usize::from(draw(seed, t, c.id, 7) < offered.fract());
        for j in 0..n {
            let actual_s =
                ((c.duration_s as f64 * (0.8 + draw(seed, t, c.id, 31 + j as u64) * 0.4)).ceil()
                    as u64)
                    .max(1);
            let id = ((t + 10000) as u64) * 1000 + c.id as u64 * 100 + j as u64;
            let a = Arrival {
                id,
                client: c.id,
                cpu: c.cpu,
                memory_gib: c.memory_gib,
                estimated_s: c.duration_s,
                actual_s,
            };
            values[0] += 1.;
            values[1] += (c.cpu as u64 * c.duration_s) as f32;
            values[2] += (c.memory_gib as u64 * c.duration_s) as f32;
            jobs.push(a);
        }
    }
    Bucket {
        at_ms: t * 1000,
        values,
        jobs,
    }
}
pub fn prehistory(seed: u64, s: Scenario, clients: &[Client]) -> Vec<Bucket> {
    (-(CONTEXT as i64) + 1..=0)
        .map(|t| bucket(seed, s, clients, t))
        .collect()
}
pub fn input(history: &[Bucket], origin_ms: u64) -> Input {
    let rows = &history[history.len().saturating_sub(CONTEXT)..];
    Input {
        interval_ms: 1000,
        origin_ms,
        timestamps: rows.iter().map(|b| EPOCH + b.at_ms / 1000).collect(),
        values: rows.iter().map(|b| b.values.to_vec()).collect(),
        prediction_length: HORIZON,
    }
}
