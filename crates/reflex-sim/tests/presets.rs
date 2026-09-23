// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex_sim::{
    playground::{self, inference::JevSettings, IncidentScenario},
    scheduler,
};

async fn breaker(scenario: IncidentScenario, step: u64) -> playground::Session {
    let mut s = playground::Session::new(42).unwrap();
    s.command(playground::Command::Scenario { scenario })
        .await
        .unwrap();
    s.command(playground::Command::Play).await.unwrap();
    let mut at = 0;
    while at < 160_000 {
        let delta = step.min(160_000 - at);
        s.tick(delta as f64).await.unwrap();
        at += delta;
    }
    s.command(playground::Command::Pause).await.unwrap();
    s
}
#[tokio::test]
async fn circuit_presets_apply_at_exact_times_and_replay_without_duplicate_injections() {
    for scenario in [
        IncidentScenario::SlowdownSurge,
        IncidentScenario::CyclicPressure,
    ] {
        let mut a = breaker(scenario, 1000).await;
        let b = breaker(scenario, 137).await;
        let before = serde_json::to_value(a.view()).unwrap();
        let other = serde_json::to_value(b.view()).unwrap();
        assert_eq!(before["injections"], other["injections"]);
        assert_eq!(before["counts"], other["counts"]);
        let mut times: Vec<_> = before["injections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["at_ms"].as_f64().unwrap() as u64)
            .collect();
        times.sort();
        assert_eq!(
            times,
            if scenario == IncidentScenario::SlowdownSurge {
                vec![75_000, 120_000]
            } else {
                vec![30_000, 60_000, 90_000, 150_000]
            }
        );
        if scenario == IncidentScenario::CyclicPressure {
            assert_eq!(a.view().simulation.horizon_ms, 600_000.);
            assert!(a.view().simulation.services[1].faults.surge);
        } else {
            assert!(a
                .view()
                .simulation
                .services
                .iter()
                .all(|s| !s.faults.slow && !s.faults.errors && !s.faults.surge));
        }
        a.command(playground::Command::Replay).await.unwrap();
        while !a.view().paused {
            a.tick(1000.).await.unwrap();
        }
        let replay = serde_json::to_value(a.view()).unwrap();
        assert_eq!(before["counts"], replay["counts"]);
        assert_eq!(before["injections"], replay["injections"]);
        a.command(playground::Command::Reset).await.unwrap();
        assert_eq!(a.view().scenario, scenario);
        assert_eq!(a.view().simulation.at_ms, 0.);
    }
}
#[tokio::test]
async fn cyclic_pressure_repeats_and_recovers_through_the_forecast_learning_period() {
    let mut session = playground::Session::new(42).unwrap();
    session
        .command(playground::Command::Scenario {
            scenario: IncidentScenario::CyclicPressure,
        })
        .await
        .unwrap();
    session.command(playground::Command::Play).await.unwrap();
    let mut all_transitions = Vec::new();
    for second in 0..600 {
        session.tick(1000.).await.unwrap();
        all_transitions.extend(
            session
                .view()
                .simulation
                .transitions
                .into_iter()
                .filter(|t| t.at_ms > second as f64 * 1000.),
        );
    }
    let view = session.view();
    assert_eq!(view.simulation.at_ms, 600_000.);
    assert!(view.paused);
    assert_eq!(view.simulation.injections.len(), 15);
    let mut times: Vec<_> = view
        .simulation
        .injections
        .iter()
        .map(|e| e.at_ms as u64)
        .collect();
    times.sort();
    for cycle in 0..5 {
        assert_eq!(
            &times[cycle * 3..cycle * 3 + 3],
            &[
                cycle as u64 * 120_000 + 30_000,
                cycle as u64 * 120_000 + 60_000,
                cycle as u64 * 120_000 + 90_000
            ]
        );
    }
    assert_eq!(
        view.simulation.services[1].state.phase,
        reflex_sim::policy::CircuitPhase::Closed
    );
    // Verify the scripted environment repeatedly creates and releases pressure.
    // Model decisions remain unscripted; this test uses the classical policy.
    for cycle in 0..5 {
        let start = cycle as f64 * 120_000.;
        let transitions: Vec<_> = all_transitions
            .iter()
            .filter(|t| t.service == 1 && t.at_ms >= start && t.at_ms < start + 120_000.)
            .collect();
        assert!(transitions
            .iter()
            .any(|t| t.to == reflex_sim::policy::CircuitPhase::Open));
        assert!(transitions.iter().any(
            |t| t.to == reflex_sim::policy::CircuitPhase::Closed && t.at_ms >= start + 90_000.
        ));
    }
}
async fn schedule(scenario: scheduler::Scenario, step: u64) -> scheduler::Session {
    let mut s = scheduler::Session::new(42, None, JevSettings::default()).unwrap();
    s.command(scheduler::Command::Scenario { scenario })
        .await
        .unwrap();
    s.command(scheduler::Command::Play).await.unwrap();
    let mut at = 0;
    while at < 140_000 {
        let dt = step.min(140_000 - at);
        s.tick(dt).await.unwrap();
        at += dt;
    }
    s
}
#[tokio::test]
async fn scheduling_presets_are_repeatable_and_restore_original_client_profiles_on_reset() {
    for scenario in [
        scheduler::Scenario::TrafficBurst,
        scheduler::Scenario::ResourceMix,
    ] {
        let mut a = schedule(scenario, 1000).await;
        let b = schedule(scenario, 137).await;
        assert_eq!(
            serde_json::to_value(a.data()).unwrap(),
            serde_json::to_value(b.data()).unwrap()
        );
        if scenario == scheduler::Scenario::ResourceMix {
            assert!(a.data().jobs.iter().any(|j| j.client == 1
                && j.cpu == 8
                && j.memory_gib == 2
                && j.arrived_at >= 75000
                && j.arrived_at < 125000));
            assert!(a
                .data()
                .jobs
                .iter()
                .any(|j| j.client == 2 && j.memory_gib == 24));
        } else {
            assert!(a.view().clients.iter().all(|c| c.config.rate == 1.));
        }
        a.command(scheduler::Command::Reset).await.unwrap();
        assert!(a.view().clients.iter().all(|c| c.config.rate == 2.));
        assert_eq!(a.view().scenario, scenario);
    }
}
