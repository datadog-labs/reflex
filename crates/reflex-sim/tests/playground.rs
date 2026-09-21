use reflex_sim::{
    engine::live::{Faults, LiveSimulation, HORIZON_MS},
    playground::{Command, Session},
    policy::CircuitPhase,
};

#[tokio::test]
async fn live_faults_change_existing_work_and_repair_preserves_backlog() {
    let mut live = LiveSimulation::new(42).unwrap();
    live.advance_to(5000.0).await.unwrap();
    let before = live.view();
    live.set_faults(
        1,
        Faults {
            slow: true,
            errors: true,
            surge: true,
        },
    )
    .unwrap();
    assert_eq!(live.view().services[1].state.latency_multiplier, 6.0);
    assert_eq!(live.view().services[1].state.offered_rate, 56.0);
    live.advance_to(12000.0).await.unwrap();
    let broken = live.view();
    assert!(broken.counts.timeout > before.counts.timeout);
    assert!(broken
        .transitions
        .iter()
        .any(|t| t.service == 1 && t.to == CircuitPhase::Open));
    assert!(broken.services[1].state.stress > 0.1);
    assert!(broken.post_timeout_work_ms > 0.0);
    let queue = broken.services[1].state.queued;
    let stress = broken.services[1].state.stress;
    live.repair_all().unwrap();
    assert_eq!(live.view().services[1].state.queued, queue);
    assert_eq!(live.view().services[1].state.stress, stress);
    assert_eq!(live.view().services[1].state.latency_multiplier, 1.0);
    live.advance_to(60000.0).await.unwrap();
    let recovered = live.view();
    assert_eq!(recovered.services[1].state.phase, CircuitPhase::Closed);
    assert!(recovered.services[1].state.stress < stress);
    assert_eq!(recovered.services[1].timed_out_work, 0);
}

#[tokio::test]
async fn unrelated_downstreams_receive_identical_traffic_when_one_service_surges() {
    let mut control = LiveSimulation::new(99).unwrap();
    let mut surge = LiveSimulation::new(99).unwrap();
    surge
        .set_faults(
            1,
            Faults {
                surge: true,
                ..Default::default()
            },
        )
        .unwrap();
    control.advance_to(10000.0).await.unwrap();
    surge.advance_to(10000.0).await.unwrap();
    let a = control.view();
    let b = surge.view();
    assert!(b.services[1].state.counts.offered > 3 * a.services[1].state.counts.offered);
    for i in [0, 2] {
        assert_eq!(
            serde_json::to_value(&a.services[i].state.counts).unwrap(),
            serde_json::to_value(&b.services[i].state.counts).unwrap()
        );
    }
}

#[tokio::test]
async fn pause_step_reset_and_replay_reproduce_incident_outcomes() {
    let mut session = Session::new(42).unwrap();
    session.tick(1000.0).await.unwrap();
    assert_eq!(session.view().simulation.at_ms, 0.0);
    session.command(Command::Step).await.unwrap();
    assert_eq!(session.view().simulation.at_ms, 1000.0);
    assert!(session.view().paused);
    session
        .command(Command::Fault {
            service: 1,
            faults: Faults {
                slow: true,
                surge: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    session.command(Command::Play).await.unwrap();
    for _ in 0..15 {
        session.tick(1000.0).await.unwrap();
    }
    session.command(Command::Repair).await.unwrap();
    for _ in 0..25 {
        session.tick(1000.0).await.unwrap();
    }
    session.command(Command::Pause).await.unwrap();
    let original = session.view();
    session.command(Command::Replay).await.unwrap();
    assert!(session
        .command(Command::Fault {
            service: 0,
            faults: Faults::default()
        })
        .await
        .is_err());
    // A different ticking cadence must not change the incident's traffic or outcomes.
    for _ in 0..100 {
        session.tick(500.0).await.unwrap();
    }
    let replay = session.view();
    assert!(replay.paused);
    assert!(!replay.replaying);
    assert_eq!(original.simulation.at_ms, replay.simulation.at_ms);
    assert_eq!(
        serde_json::to_value(original.simulation.counts).unwrap(),
        serde_json::to_value(replay.simulation.counts).unwrap()
    );
    assert_eq!(
        original.simulation.transitions.len(),
        replay.simulation.transitions.len()
    );
    for (a, b) in original
        .simulation
        .transitions
        .iter()
        .zip(&replay.simulation.transitions)
    {
        assert_eq!((a.service, a.from, a.to), (b.service, b.from, b.to));
        assert!((a.at_ms - b.at_ms).abs() < 1e-6);
    }
    session.command(Command::Reset).await.unwrap();
    assert_eq!(session.view().simulation.at_ms, 0.0);
    assert_eq!(session.view().simulation.counts.offered, 0);
    assert_eq!(session.view().simulation.seed, 42);
    assert!(session.view().paused);
}

#[tokio::test]
async fn live_clock_and_commands_are_bounded() {
    let mut live = LiveSimulation::new(42).unwrap();
    assert!(live.set_faults(3, Faults::default()).is_err());
    for at in [-1.0, f64::NAN, HORIZON_MS + 1.0] {
        assert!(live.advance_to(at).await.is_err());
    }
    let mut session = Session::new(42).unwrap();
    assert!(session.command(Command::Speed { value: 0 }).await.is_err());
    assert!(session.command(Command::Replay).await.is_err());
    session.command(Command::Speed { value: 4 }).await.unwrap();
    session.command(Command::Play).await.unwrap();
    for _ in 0..50 {
        session.tick(1000.0).await.unwrap();
    }
    assert_eq!(session.view().simulation.at_ms, HORIZON_MS);
    assert!(session.view().paused);
    assert!(session.command(Command::Play).await.is_err());
    let counts = session.view().simulation.counts;
    assert!(counts.offered >= counts.success + counts.error + counts.timeout + counts.shed);
    assert_eq!(counts.offered, counts.admitted + counts.shed);
}

#[tokio::test]
async fn replay_preserves_multiple_fault_edits_at_the_same_paused_instant() {
    let mut session = Session::new(7).unwrap();
    session.command(Command::Step).await.unwrap();
    session
        .command(Command::Fault {
            service: 1,
            faults: Faults {
                errors: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    session.command(Command::Repair).await.unwrap();
    for _ in 0..10 {
        session.command(Command::Step).await.unwrap();
    }
    let original = session.view();
    session.command(Command::Replay).await.unwrap();
    for _ in 0..11 {
        session.tick(1000.0).await.unwrap();
    }
    assert_eq!(
        serde_json::to_value(original.simulation.counts).unwrap(),
        serde_json::to_value(session.view().simulation.counts).unwrap()
    );
}
