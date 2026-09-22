# TypeSafe SDK and Reflex integration

Implementation notes for the standalone TypeSafe AI Rust SDK and its Reflex adapter. The initial implementation lives in `crates/typesafe-ai` and `crates/reflex-typesafe`; see the [SDK quickstart](SDK_README.md).

For the library's concepts and a circuit-breaker example, see [Reflex SDK design](REFLEX_SDK.md).

## Client, task, and adapter

The integration has three components:

- `TypeSafeClient` handles authentication, connections, retries, transport timeouts, response validation, and usage metadata.
- `SystemOneTask` holds a model and typed question definition. State is supplied when the task is invoked.
- `TypeSafeJudge` combines an instantiated client and task to implement Reflex's judge interface.

The standalone client follows the [TypeSafe JavaScript SDK](https://docs.typesafe.ai/sdk/javascript). It can be used independently of Reflex.

## Standalone client usage

```rust
use std::time::Duration;
use typesafe_ai::TypeSafeClient;

let client = TypeSafeClient::builder()
    .api_key(std::env::var("TYPESAFE_API_KEY")?)
    .timeout(Duration::from_secs(2))
    .build()?;

let response = client.system_one(&task, &state).await?;
```

Here, `task` is a previously constructed `SystemOneTask`, and `state` is the application's serializable input. The circuit-breaker example in the main document shows a task definition.

The `questions!` helper defines named questions with typed answers. A Choice question selects from typed option values, such as a Rust enum. For Reflex, those values are the application's action type, such as `CircuitAction`. A deliberate no-op can be an action variant such as `NoChange`.

The standalone SDK remains generic over option values and does not depend on Reflex. Choice values use their serialized string as a wire label, or JSON text for other serialized values. Duplicate labels are rejected. Responses map those labels back to cloned typed values.

## Reflex judge interface

A judge returns a typed application action from the state supplied to `controller.evaluate(&state)`, or an error if it cannot obtain a valid response.

```rust
pub struct Judgment<A> {
    pub action: A,
    pub confidence: Option<f64>,
}

pub trait Judge<S, A> {
    async fn judge(
        &self,
        state: &S,
    ) -> Result<Judgment<A>, JudgeError>;
}
```

The controller validates a successful `Judgment<A>` and wraps it in a `ProposedDecision<A>`, preserving `action` and `confidence`. The decision exposes `action()` and `confidence() -> Option<f64>`. Confidence, when present, must be finite and within `0.0..=1.0`; invalid values become `EvaluationError` before a proposal is created. `None` remains missing confidence, not zero and not an evaluation failure. Judge failures and controller timeouts become `EvaluationError` values. Evaluation returns `Result<ProposedDecision<A>, EvaluationError>`, which the application passes intact to its executor.

The adapter retains the latest successful provider response's distribution, model, usage, request ID, and confidence in `last_response()`. Concurrent requests can replace this diagnostic slot; it is not per-decision storage.

## Confidence and domain scores

TypeSafe Choice answers contain the selected option, its answer distribution, and confidence. Score answers measure a position along application-defined levels and carry their own distribution and confidence. The standalone SDK should preserve those distinct answer shapes. See [TypeSafe question primitives](https://docs.typesafe.ai/primitives).

TypeSafe's confidence summarizes the shape of the answer distribution; it is not the domain score or a calibrated probability of execution success. The adapter forwards it without inventing confidence when absent or combining confidence from independent answers. See [TypeSafe confidence](https://docs.typesafe.ai/confidence).

The core controller does not apply a confidence threshold. A state-machine action row may declare `min_confidence`; the executor rejects a low value with `insufficient_confidence` or a missing required value with `missing_confidence`. These are execution-policy rejections, not evaluation failures, and do not trigger `evaluation_error(...)` rules.

The initial Reflex integration selects an action answer and its confidence. Domain-specific scores need explicitly named, typed values and defined scales if introduced later; there is no generic core score field. Explicit abstention remains a separate design question.

## Inject an instantiated client

```rust
use reflex_typesafe::TypeSafeJudge;

let judge = TypeSafeJudge::new(client, task)
    .select_answer(|answers| answers.action);
```

This example assumes a task whose named `action` question returns the application's action type `A`. `select_answer` selects the complete typed answer, including confidence and distribution. The adapter packages the chosen action and confidence into `Judgment<A>`. The application passes the judge to `Controller::builder().judge(judge)`.

The client handles transport and decoding. The controller enforces the overall inference deadline, including SDK retries, and reports a proposed decision or evaluation error. The configured executor receives that result. The library-provided state-machine executor handles actions and evaluation failures through declared transition rules, guards, and validated state updates; applications can also supply a custom executor. Error mapping should preserve the information the executor needs to distinguish failures.

## Package boundaries

| Package | Responsibility |
| --- | --- |
| `typesafe-ai` | Standalone TypeSafe client and typed API requests/responses |
| `reflex` | Typed actions, judge, controller, proposed decisions, evaluation errors, executor interface, declarative state-machine execution, and outcomes |
| `reflex-typesafe` | Adapter connecting an instantiated TypeSafe client and task to the judge interface |

The application combines the core library with the TypeSafe adapter. The standalone TypeSafe client can also be used independently. Packages are local workspace crates; publication is disabled during API development.

## Implemented behavior and remaining scope

`questions!` creates named, heterogeneous typed answers. Choice, Score, and Noul are supported. The client validates question definitions, answer names and types, choice labels, probability distributions, confidence ranges, score bounds and legends, and usage metadata.

The client defaults to a ten-second overall deadline and two retries for selected transient HTTP statuses. Redirects are disabled. Custom endpoints require HTTPS except for loopback HTTP test servers. The adapter maps failures to judge codes such as `typesafe_http_401`, `typesafe_timeout`, and `typesafe_invalid_response`; the controller's own deadline produces `EvaluationError::Timeout`.

The `Judge` trait returns a `Send` future and can be implemented with an async method. `TypeSafeJudge` requires serializable, thread-safe input state. `select_answer` selects a complete Choice answer; Score and Noul remain available to standalone SDK consumers. Additional policy composition and per-decision diagnostic storage remain future work.

## References

- [TypeSafe JavaScript SDK](https://docs.typesafe.ai/sdk/javascript)
- [TypeSafe question primitives](https://docs.typesafe.ai/primitives)
- [TypeSafe HTTP API](https://docs.typesafe.ai/api)
