# vyges-events

Canonical **structured events** for Vyges loom engines and tools — the shared *producer*
side of the logging/event contract (`vyges-events/1.0`). It exists so that in LLM-driven
MCP orchestration there is a uniform, queryable **causal trail** to debug with and for the
model to trace back *why* something happened.

- **Schema co-located & self-published.** The `vyges-events/1.0` JSON Schema lives *in this
  crate* (`schema/vyges-events-1.0.json`, embedded as [`vyges_events::EVENTS_SCHEMA`]), and the
  `vyges-events --schema` binary dumps it — the events analog of a tool's `--describe`. One
  owner, no drift. Cross-language consumers (e.g. the Python root-cause pipeline) validate
  against that one artifact.
- **Daemonless.** Each engine emits JSONL **locally** (default: stderr). The orchestrator
  (vyges-cli MCP `invoke`) aggregates per-run into the causal trail — there is no central
  logging service.
- **`tracing`-based.** With the default `tracing-bridge` feature, engines emit via standard
  macros and get a canonical event for free.
- **Logs and events are one path** (not two systems). A plain progress log is just an
  `info`/`debug` event with no `code`/`objects`. Filter by severity with `VYGES_LOG=warn`; the
  *same* event renders as pretty text at a terminal or JSONL when piped — auto (TTY vs pipe), or
  force with `VYGES_LOG_FORMAT=json|text`. So the machine causal-trail and the human log are the
  same records, two views.

## Use it (engine side)

```rust
use tracing::warn;

fn main() {
    vyges_events::tracing_bridge::init("vyges-drc");
    // ... one line per event; code + objects are the cross-stage keys ...
    warn!(code = "DRC-0142", objects = "net:data[3],macro:sram0", "spacing < min");
}
```

Or emit an `Event` directly:

```rust
use vyges_events::{Event, Severity, emit};
emit(&Event::new("vyges-drc", Severity::Warn, "spacing < min")
        .with_code("DRC-0142")
        .with_objects(vec!["net:data[3]".into()]));
```

## The event (`vyges-events/1.0`)

```jsonc
{ "schema":"vyges-events/1.0", "ts_ms":1720000000000, "run_id":"...", "stage":"route",
  "step_index":9, "tool":"vyges-drc", "severity":"warn", "code":"DRC-0142",
  "msg_template":"spacing < min on %s", "raw_msg":"spacing < min on net data[3]",
  "objects":["net:data[3]","macro:sram0"], "file":"x.sv:42" }
```

`code` (severity+ID) is the clustering key; `objects` is the **cross-stage co-reference** key
that links an early warning to a late failure — the substrate the root-cause pipeline consumes.

## Get the schema

```sh
vyges-events --schema        # dumps schema/vyges-events-1.0.json
```

## Where it fits

- **Producer:** loom engines depend on this crate to emit.
- **Consumer:** `vyges-cli` (MCP `invoke`) depends on it for the types and does streaming +
  per-run aggregation + external-tool (OpenROAD) normalization into the same schema.
- **Root-cause:** `soc-generator` / Sley (Python) reads the emitted JSONL against the published
  schema. See `soc-generator/docs/architecture/flow-log-root-cause-analysis.md`.

Apache-2.0.
