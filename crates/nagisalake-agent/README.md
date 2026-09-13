# nagisalake-agent

Bounded, provider-independent skill execution for the Nagisalake workspace.

```rust,no_run
use nagisalake_agent::{AgentService, RunRequest, RuntimeConfig, Skill, opencode::{OpenCode, OpenCodeConfig}};
use service_async::Service;

# async fn example() -> Result<(), nagisalake_agent::AgentError> {
let backend = OpenCode::new(OpenCodeConfig::default())?;
let service = AgentService::new(
    backend,
    vec![Skill { id: "h3-prompt-writing".into(), version: "1".into() }],
    RuntimeConfig::default(),
)?;
let mut run = service.call(RunRequest {
    skill: "h3-prompt-writing".into(),
    input: "Write a text-only video prompt for an origami boat.".into(),
    options: Default::default(),
}).await?;
while let Some(event) = run.recv().await {
    // Translate/forward this stable application event, never raw provider data.
    println!("{}", event.event_name());
}
# Ok(()) }
```

`AgentService<B>: Service<RunRequest>` owns allowlisting, input limits, admission,
cancellation, deadlines and exactly one terminal outcome. Implement `Backend`
with a `Send` RPITIT future to add another provider. No boxed future or trait-object
backend is required. Backends must clean up when their execution future is dropped.

The OpenCode adapter creates one session per run, denies all tools except the
approved literal skill, routes one shared SSE connection by session, and reconciles
persisted messages after missed events. It does not mistake `tool-calls` for final
completion and does not publish reasoning or tool payloads.

Modules:

- `model`: public request/progress/event/error contracts.
- `service`, `handle`: generic supervision and bounded channels.
- `opencode/client`, `config`: HTTP and secret/config boundaries.
- `opencode/router`, `sse`, `wire`: lifecycle, routing and bounded incremental parsing.
- `opencode/reconcile`, `backend`: state reconciliation and session ownership.

See [Hub/API/deployment documentation](../../docs/AGENT_API_CN.md).

Tests use local mock servers, not a configured LLM. The `opencode` example is an
explicit live smoke test using the `[opencode]` section of a Hub TOML file.
