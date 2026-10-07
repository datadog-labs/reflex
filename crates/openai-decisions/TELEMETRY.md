# OpenAI client telemetry

`openai-decisions` emits OpenTelemetry metrics and `tracing` spans/events for `DecisionsClient::decide`. The application owns providers, subscribers, exporters, resource tags, sampling, and shutdown. The library installs no global subscriber, starts no exporter, and reads no Datadog credentials. Existing callers keep working without configuring telemetry.

This covers the standalone client. Reflex execution, state storage, and simulation traffic are outside this instrumentation.

## Metrics

| Name | Type | Meaning |
| --- | --- | --- |
| `openai.decisions.client.requests` | Counter | One observation per finished HTTP attempt, including retries and interrupted attempts |
| `openai.decisions.client.request.duration` | Histogram, seconds | Attempt latency through body reading and response validation, excluding backoff |
| `openai.decisions.client.call.duration` | Histogram, seconds | Complete call latency including preparation, retries, and backoff |
| `openai.decisions.client.retry.backoff.duration` | Histogram, seconds | Actual wait, including waits interrupted by timeout or cancellation |
| `openai.decisions.client.calls.in_flight` | Observable gauge | Currently active OpenAI calls across all clients in this process |
| `openai.decisions.client.tokens` | Counter | Validated provider-reported tokens, with `direction=input` or `output` |

Request metrics have `model` (requested), `status`, `retry` (boolean), `http_status` when a response was received, and `error.type` on failure. Calls have `model`, `status`, and on failure `error.type` and `error.stage`. Backoff has `model` and `status` (`completed`, `timeout`, `cancelled`, or `panicked`). Token metrics use the resolved model. A client built with `.name("...")` adds `client:<name>` to its request, call, backoff and token metrics; unnamed clients add nothing. The process gauge has no client/model dimensions; its callback reads an atomic count at collection time, so cancellation and concurrent clients cannot leave stale values. It represents process-wide activity even when applications supply different meters.

`status` values are `success`, `http_error`, `transport_error`, `timeout`, `cancelled`, `invalid_response`, `refusal`, `configuration_error`, `serialization_error`, and `panicked` (during Rust unwinding). `refusal` is an HTTP 200 in which the provider declined to answer a question. Preparation failures have no HTTP attempt. Error stages are `preparing`, `sending`, `reading_response`, `validating_response`, or `backing_off`. No HTTP status is fabricated for transport errors or timeouts before headers arrive.

There is no separate call counter or retry counter. Use the call histogram count for completed-call volume; filter `requests` on `retry=true` for retry volume. A 429 followed by a successful retry produces two request observations and one successful call measurement. A timeout during backoff leaves the completed HTTP attempt classified as `http_error`; it adds no phantom HTTP request. HTTP 200 with invalid answers is `invalid_response`.

Reported usage is counted as soon as the model and usage fields are validated, even if typed answers later fail validation. The published response contract does not guarantee usage; missing usage is not recorded as zero, and the resolved model then falls back to the requested one. Requests with no usable response may still incur provider charges that cannot be measured here. These metrics do not calculate dollar cost.

Timing uses `std::time::Instant`, independent of simulation time and Tokio's test clock. The existing client deadline still starts after synchronous input preparation; total latency includes that preparation. A future that is never polled produces no call. Dropping a polled future records cancellation and releases in-flight accounting. Process termination cannot guarantee final telemetry delivery.

## Traces and logs

`openai.decisions.decide` is a child of the application's current span. Each attempt is a child span named `openai.decisions.http_attempt`. Fields include requested/resolved model, timeout, retry limit, attempt count, status, provider request IDs, HTTP status, token usage, and failure stage. The call carries the final attempt's response metadata; each attempt retains its own. The retry-exhausted flag distinguishes a retryable HTTP response with no remaining retry budget.

WARN events describe retry scheduling and terminal failure; DEBUG events describe stage changes. No raw input state, question text, answers, credentials, endpoint query strings, HTTP bodies, or free-form error messages are recorded. Request IDs are trace/log fields, never metric labels. Requested and resolved model names are metric labels, so keep model names stable.

Metrics are recorded independently of trace sampling. Cancellation is tagged but does not set the span's OpenTelemetry error status. Non-cancellation failures do. The instrumentation does not change retries, returned errors, deadlines, or response validation.

## Connect an application-owned meter

```rust
use opentelemetry::metrics::MeterProvider;
use openai_decisions::DecisionsClient;

let client = DecisionsClient::builder()
    .api_key(std::env::var("OPENAI_API_KEY")?)
    .meter(meter_provider.meter("openai-decisions"))
    .build()?;
```

`meter_provider` is your configured OpenTelemetry provider. Without `.meter(...)`, the client gets a meter from the global provider **at client construction time**; initialize that provider first. Reuse clients to reuse connections and instruments. Tracing uses the application's current subscriber. Applications already using `tracing` should add layers to their existing subscriber rather than install a second global subscriber.

The library uses OpenTelemetry API 0.33. The runnable example uses matching SDK/exporters and `opentelemetry-appender-tracing` 0.33 with `tracing-opentelemetry` 0.34. The tracing layer activates the current OpenTelemetry span context for correlated logs. Update these dependencies together so providers, exporters, and bridges share the same API version. Exporters, the OpenTelemetry SDK, and these bridges are development dependencies, not runtime dependencies of the client library.

## Export directly to Datadog

The runnable [example](examples/datadog.rs) uses the application setup in [support/datadog.rs](examples/support/datadog.rs). It creates three HTTP/protobuf exporters and sends directly to `https://otlp.<DD_SITE>/v1/metrics`, `/v1/traces`, and `/v1/logs`. No Agent is needed.

Configure these variables in your shell (or source a local, untracked environment file):

```dotenv
DD_API_KEY=your-datadog-api-key
DD_SITE=datadoghq.com
DD_SERVICE=openai-decisions-example
DD_ENV=local
DD_VERSION=0.1.0
OPENAI_API_KEY=your-openai-api-key
OPENAI_DECISIONS_MODEL=gpt-6-luna
```

`DD_SITE` must match your Datadog organization. The example supports the listed commercial Datadog sites in its source. Run from the workspace root:

```sh
cargo run -p openai-decisions --example datadog
```

This command makes **one live model call**, with the client's normal retry policy, and exports real telemetry. It uses synthetic input, may incur OpenAI and Datadog charges, and prints the selected action. The example does not load `.env` files automatically or change the running playground.

The setup explicitly selects delta temporality for counters/histograms, maps resource attributes into metric tags, maps histograms to distributions, and adds `compute_stats=true` for Datadog trace metrics. In-flight activity is a gauge, avoiding cumulative sum export. Service, environment, and version are resource attributes. The exporter authenticates with the `dd-api-key` header; no Datadog application key is needed.

Metrics export every ten seconds. Traces and logs use background batches. Shutdown flushes all providers before exit, including after a failed model call. Initialize this example's blocking HTTP exporters before entering the Tokio runtime and shut them down outside it; their network work runs on SDK background threads. Telemetry delivery is best effort, with bounded exporter timeouts and SDK queues. Export failures do not change a client result. This example is a starting point for application setup, not a durable telemetry spool.

The subscriber exports `openai_decisions` and `reflex` events/spans at INFO and above to avoid recursively collecting exporter transport logs. Application owners can extend the filter for their own modules. In Datadog, filter on your `service`/`env`, inspect `openai.decisions.client.*` metrics, and find `openai.decisions.decide` traces and correlated retry/failure logs.

## Verification

```sh
cargo test -p openai-decisions --all-targets
```

Tests use loopback model/intake servers and in-memory telemetry. They check counts, timing boundaries, retries, malformed responses, preparation errors, deadlines, cancellation, concurrency, trace parenting, log correlation, and payload exclusion. The exporter test decodes actual OTLP protobuf payloads for all three signals and verifies authentication, resource tags, and delta temporality. It needs no real keys and does not contact Datadog or OpenAI.

References: [Datadog direct intake](https://docs.datadoghq.com/opentelemetry/setup/otlp_ingest/), [metrics](https://docs.datadoghq.com/opentelemetry/setup/otlp_ingest/metrics/), [traces](https://docs.datadoghq.com/opentelemetry/setup/otlp_ingest/traces/), [logs](https://docs.datadoghq.com/opentelemetry/setup/otlp_ingest/logs/).
