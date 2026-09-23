// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::{Client, ClientConfig};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    #[default]
    Sandbox,
    TrafficBurst,
    ResourceMix,
    Cyclical,
}
impl Scenario {
    pub(super) fn times(self, remote: bool) -> Vec<u64> {
        let scale = if remote { 3 } else { 1 };
        match self {
            Self::Cyclical => (1..=120).map(|i| i * 5000).collect(),
            Self::Sandbox => vec![],
            Self::TrafficBurst => vec![75_000 * scale, 115_000 * scale],
            Self::ResourceMix => vec![75_000 * scale, 125_000 * scale],
        }
    }
    pub(super) fn description(self, remote: bool) -> String {
        let k = if remote { 3 } else { 1 };
        match self {
            Self::Cyclical=>"Whole-number demand waves repeat every 60s for six minutes (ten in Datadog mode). Local forecasts use observed 10-second mean demand, after three real cycles (180s). Compare a forecast from 180s or later against the following cycles; Toto never sees the schedule.".into(),
            Self::Sandbox=>"Set each client's traffic and job sizes. No scheduled changes.".into(),
            Self::TrafficBurst=>format!("Three clients start at 2 jobs/s each. At {}s each jumps to 6 jobs/s; at {}s arrivals ease to 1 job/s. Watch the queue build and drain.",75*k,115*k),
            Self::ResourceMix=>format!("At {}s, Client 2 requests CPU-heavy jobs (8 CPU / 2 GiB) and Client 3 requests memory-heavy jobs (2 CPU / 24 GiB). Original sizes return at {}s; arrival rates stay fixed.",75*k,125*k),
        }
    }
}
pub(super) fn clients() -> Vec<Client> {
    [(1, 2, 4000), (3, 6, 8000), (6, 12, 12000)]
        .into_iter()
        .enumerate()
        .map(|(i, (cpu, memory_gib, duration_ms))| {
            let config = ClientConfig {
                rate: 2.,
                cpu,
                memory_gib,
                duration_ms,
                enabled: true,
            };
            Client {
                priority: super::Priority::Normal,
                id: i as u64,
                name: format!("Client {}", i + 1),
                next_at: config.next(0),
                config,
                sequence: 0,
            }
        })
        .collect()
}
pub(super) fn update(scenario: Scenario, stage: usize, c: &mut Client) {
    match scenario {
        Scenario::TrafficBurst => c.config.rate = if stage == 0 { 6. } else { 1. },
        Scenario::ResourceMix => {
            if stage == 0 {
                match c.id {
                    1 => {
                        c.config.cpu = 8;
                        c.config.memory_gib = 2;
                    }
                    2 => {
                        c.config.cpu = 2;
                        c.config.memory_gib = 24;
                    }
                    _ => {}
                }
            } else if let Some(initial) = clients().into_iter().find(|a| a.id == c.id) {
                c.config.cpu = initial.config.cpu;
                c.config.memory_gib = initial.config.memory_gib;
            }
        }
        Scenario::Cyclical => {
            let seconds = (stage + 1) as f64 * 5.;
            c.config.rate = cycle_rate(seconds);
        }
        Scenario::Sandbox => {}
    }
}

pub(super) fn cycle_rate(seconds: f64) -> f64 {
    (2. + (std::f64::consts::TAU * seconds / 60.).sin()).round()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        playground::inference::JevSettings,
        scheduler::{Command, Session},
    };
    #[tokio::test]
    async fn cyclic_arrivals_repeat_across_tick_sizes_and_use_observed_warmup() {
        async fn run(step: u64) -> Session {
            let mut s = Session::new(42, None, JevSettings::default()).unwrap();
            s.command(Command::Scenario {
                scenario: Scenario::Cyclical,
            })
            .await
            .unwrap();
            assert_eq!(s.forecast.local_min_samples, 180);
            assert_eq!(s.view().horizon_ms, 360_000);
            s.command(Command::Play).await.unwrap();
            let mut at = 0;
            while at < 300_000 {
                let dt = step.min(300_000 - at);
                s.tick(dt).await.unwrap();
                at += dt;
            }
            s
        }
        let mut a = run(1000).await;
        let b = run(137).await;
        assert_eq!(
            serde_json::to_value(a.data()).unwrap(),
            serde_json::to_value(b.data()).unwrap()
        );
        assert!(a.data().jobs.len() > 100);
        a.command(Command::Reset).await.unwrap();
        assert_eq!(a.view().at_ms, 0);
        assert!(a
            .view()
            .clients
            .iter()
            .all(|c| c.config.rate == 2.));
        a.command(Command::Scenario {
            scenario: Scenario::Sandbox,
        })
        .await
        .unwrap();
        assert_eq!(a.forecast.local_min_samples, 64);
    }
}
