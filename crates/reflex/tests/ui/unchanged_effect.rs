use reflex::state_machine;
#[derive(Clone, PartialEq)]
enum Phase { Idle }
fn main() {
    let _ = state_machine! {
        phase: Phase, data: (), action: (), event: (),
        transitions: [Phase::Idle + action(()) => unchanged { effect: |()| async {} }],
    };
}
