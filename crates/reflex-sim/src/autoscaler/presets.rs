// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::engine::{draw, Control, HORIZON_MS};

/// Datadog evidence arrives tens of seconds late and Toto needs 320 seconds of queried
/// history, so a run in that mode lasts longer and its scheduled changes come later.
pub const DATADOG_HORIZON_MS: u64 = 900_000;
const DATADOG_TIME_SCALE: u64 = 2;
pub fn horizon_ms(remote: bool) -> u64 {
    if remote {
        DATADOG_HORIZON_MS
    } else {
        HORIZON_MS
    }
}
use serde::{Deserialize, Serialize};

pub const CYCLE_MS: u64 = 120_000;
/// Demand ramps up from this offset in each cycle and back down from `CYCLE_PEAK_END_MS`.
pub const CYCLE_PEAK_START_MS: u64 = 40_000;
pub const CYCLE_PEAK_END_MS: u64 = 80_000;
const CYCLE_STEP_MS: u64 = 5_000;
const WEB: usize = 0;
const API: usize = 1;
const BATCH: usize = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    #[default]
    Live,
    SurgeRecovery,
    MemoryHeavyBurst,
    CyclicalLoad,
}
/// A scheduled change to load. Jev never sees the schedule, only its effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Change {
    Control(Control),
    Replicas { workload: usize, replicas: u32 },
}
impl Scenario {
    /// Scheduled changes. With Datadog evidence the two event presets run at half speed, so
    /// telemetry has warmed up before the first change; the cycle keeps its length and
    /// simply repeats for longer.
    pub fn script(self, seed: u64, remote: bool) -> Vec<(u64, Change)> {
        let surge =
            |workload, enabled| Change::Control(Control::ReplicaSurge { workload, enabled });
        let heavy = |workload, enabled| Change::Control(Control::MemoryHeavy { workload, enabled });
        let scale = if remote { DATADOG_TIME_SCALE } else { 1 };
        let events: Vec<(u64, Change)> = match self {
            Self::Live => vec![],
            Self::SurgeRecovery => vec![
                (45_000, surge(WEB, true)),
                (90_000, surge(API, true)),
                (240_000, surge(WEB, false)),
                (300_000, surge(API, false)),
            ],
            Self::MemoryHeavyBurst => vec![
                (45_000, heavy(API, true)),
                (150_000, heavy(BATCH, true)),
                (330_000, heavy(API, false)),
                (390_000, heavy(BATCH, false)),
            ],
            Self::CyclicalLoad => {
                let mut script = Vec::new();
                let mut current = [cycle_replicas(WEB, 0, seed), cycle_replicas(API, 0, seed)];
                for at in (CYCLE_STEP_MS..horizon_ms(remote)).step_by(CYCLE_STEP_MS as usize) {
                    for workload in [WEB, API] {
                        let replicas = cycle_replicas(workload, at, seed);
                        if replicas != current[workload] {
                            current[workload] = replicas;
                            script.push((at, Change::Replicas { workload, replicas }));
                        }
                    }
                }
                return script;
            }
        };
        events
            .into_iter()
            .map(|(at, change)| (at * scale, change))
            .collect()
    }
    pub fn description(self, remote: bool) -> String {
        let k = if remote { DATADOG_TIME_SCALE } else { 1 };
        match self {
            Self::Live => "Drive the load yourself: surge a workload, make its pods memory-heavy, or put a node group out of stock. No scheduled changes.".into(),
            Self::SurgeRecovery => format!("web triples its replicas at {}s and api at {}s; the surges end at {}s and {}s. Watch pods wait for nodes, then watch which nodes can safely be removed.", 45 * k, 90 * k, 240 * k, 300 * k),
            Self::MemoryHeavyBurst => format!("api pods need four times the memory from {}s and batch pods from {}s, until {}s and {}s. Only memory-heavy nodes can hold them: any other group leaves them pending.", 45 * k, 150 * k, 330 * k, 390 * k),
            Self::CyclicalLoad if remote => "Every two minutes web and api ramp up for 40 seconds, then fall back, for 15 minutes. Toto forecasts from this run's metrics queried back from Datadog: it needs 320 seconds of that history, which arrives about 30 seconds late, so the first ramps are met reactively. Toto never sees the schedule.".into(),
            Self::CyclicalLoad => "Every two minutes web and api ramp up for 40 seconds, then fall back. Nodes take about 30 seconds to start, so reacting is too late; once Toto has seen the pattern, Jev can provision ahead of the ramp. Toto never sees the schedule.".into(),
        }
    }
    /// Observed history Toto needs before its first forecast.
    pub fn forecast_min_samples(self) -> usize {
        if self == Self::CyclicalLoad {
            180
        } else {
            64
        }
    }
}
/// Replicas a workload wants at a point in the repeating cycle. The peak height varies a
/// little from cycle to cycle with the seed.
pub fn cycle_replicas(workload: usize, at_ms: u64, seed: u64) -> u32 {
    let (low, high) = match workload {
        WEB => (4, 9 + (draw(seed, 0x6379636c, at_ms / CYCLE_MS) % 3) as u32),
        API => (2, 4),
        _ => return 1,
    };
    let phase = at_ms % CYCLE_MS;
    let middle = (low + high) / 2;
    // One intermediate step on the way up and on the way down.
    if (CYCLE_PEAK_START_MS + CYCLE_STEP_MS..CYCLE_PEAK_END_MS).contains(&phase) {
        high
    } else if (CYCLE_PEAK_START_MS..CYCLE_PEAK_END_MS + CYCLE_STEP_MS).contains(&phase) {
        middle
    } else {
        low
    }
}
