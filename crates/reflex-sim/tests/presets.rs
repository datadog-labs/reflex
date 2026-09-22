use reflex_sim::{
    playground::{self, inference::JevSettings, IncidentScenario},
    recovery, scheduler,
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
        IncidentScenario::ErrorWaves,
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
                vec![75_000, 100_000, 125_000, 150_000]
            }
        );
        assert!(a
            .view()
            .simulation
            .services
            .iter()
            .all(|s| !s.faults.slow && !s.faults.errors && !s.faults.surge));
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
#[tokio::test]
async fn recovery_presets_restart_with_known_traffic_and_bandwidth() {
    let mut s = recovery::Session::new(42, None, JevSettings::default()).unwrap();
    s.command(recovery::Command::Scenario {
        scenario: recovery::Scenario::RebuildPressure,
    })
    .await
    .unwrap();
    s.command(recovery::Command::Play).await.unwrap();
    for _ in 0..20 {
        s.tick(1000).await.unwrap();
    }
    assert!(s.data().clients.iter().all(|c| c.config.rate == 25.));
    s.command(recovery::Command::Reset).await.unwrap();
    assert!(s
        .data()
        .clients
        .iter()
        .all(|c| c.config.rate == 8. && c.config.cost_ms == 150));
    assert_eq!(s.data().bandwidth_limit, 12.);
    assert_eq!(s.view().scenario, recovery::Scenario::RebuildPressure);
}
