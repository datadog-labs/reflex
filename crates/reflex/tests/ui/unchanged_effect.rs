// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex::state_machine;
#[derive(Clone, PartialEq)]
enum Phase { Idle }
fn main() {
    let _ = state_machine! {
        phase: Phase, data: (), action: (), event: (),
        transitions: [Phase::Idle + action(()) => unchanged { effect: |()| async {} }],
    };
}
