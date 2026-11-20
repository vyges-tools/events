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

## Reference-format messages (`vyges_events::logger`)

For an engine whose text log is scored line by line, `Logger` writes each message as
`[INFO GRT-0001] text` / `[WARNING RSZ-0062] text` / `[DEBUG CTS-tree] text`, with the
bookkeeping that goes with it:

- each `TOOL-NNNN` code prints at most 1000 times, then one `message limit (1000) reached`
  line, then nothing; `suppress` / `unsuppress` per code;
- warnings and errors are counted even when they do not print; `summary()` gives the totals
  and a count per warning code (also as `flow__warnings__*` metric pairs);
- debug groups per tool + group + level, built only when they print;
- `report` for plain text; `error` / `critical` stop the run with their code;
- the lines are kept in order (`lines`, `take_lines`) for tests and comparison, and can be
  echoed to stdout and emitted as events (`echo`, `events`).

```rust
use vyges_events::logger::Logger;
let mut log = Logger::new().echo(true).events("vyges-grt");
log.info("GRT", 1, "Min routing layer: met1");
log.set_debug_level("GRT", "maze", 1);
log.debug("GRT", "maze", 1, || format!("net {} rerouted", "n1"));
```

## Debug flags

One set of environment variables for every engine. All are off by default; with none of them
set, an engine's output is unchanged.

| variable | effect |
| --- | --- |
| `VYGES_DEBUG=<tool>:<group>=<level>[,...]` | turn on debug groups, as `set_debug_level <tool> <group> <level>`; `<group>` may be `*` |
| `VYGES_STAGE_TIMES=1` | one line per engine stage: wall time, CPU time, RSS at the end, peak RSS |
| `VYGES_DEBUG_DIR=<dir>` | large traces and dumps go to `<dir>/<tool>/` |

`VYGES_DEBUG` (e.g. `VYGES_DEBUG=grt:incr=1,sta:*=2`) is parsed once and applied to every
`Logger` when it is built; the engine's own `set_debug_level` (Tcl or job file) applies after
it. Tool names match in any case; group names exactly. A group's own level wins over its tool's
`*`. A malformed entry is skipped with one warning; the rest apply. A disabled group costs one
lookup: the message closure is not called.

```rust
use vyges_events::debug::{warn_unknown_debug_groups, render_debug_groups, DebugGroup};

/// The engine's groups, named after the reference's debug groups where one exists, else `vy_`.
const DEBUG_GROUPS: &[DebugGroup] = &[
    DebugGroup { group: "incr", max_level: 2, what: "incremental reroute", reference: Some("GRT incr") },
    DebugGroup { group: "vy_cost", max_level: 1, what: "edge costs per net", reference: None },
];

warn_unknown_debug_groups("grt", DEBUG_GROUPS);        // at start-up: an unknown group warns
print!("{}", render_debug_groups("grt", DEBUG_GROUPS)); // in --describe (JSON: describe_debug_groups)

let mut log = Logger::new();                            // VYGES_DEBUG already applied
log.debug("grt", "incr", 1, || format!("net {name} ({id}) rerouted"));

let _s = vyges_events::stage("find_delays");            // reports when dropped
if let Some(dir) = vyges_events::debug_dir("grt") { /* write a dump there, log its path */ }
```

The stage line is an `info` event with code `VYGES-STAGE` and its `stage` field set:
`stage=find_delays wall_s=1.234 cpu_s=2.345 rss_mib=120.5 hwm_mib=310.2`. CPU is the whole
process (all threads), user plus system. `hwm_mib` is the process's peak resident set so far,
not reset per stage. On macOS there is no current-RSS counter: the line shows `rss_mib=n/a`,
takes the peak from `getrusage`, and says so. When `VYGES_STAGE_TIMES` is off, `stage` returns
before reading any clock or counter.

Old per-engine variables keep working while engines migrate: `register_alias("OLD_VAR", tool,
group, level)` turns the group on when the old variable is set and logs a deprecation note once.
Register aliases at the start of `main`, before building a `Logger`. `debug_enabled(tool, group,
level)` answers the same question for code without a `Logger`.

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
  per-run aggregation + external-tool normalization into the same schema.
- **Root-cause:** `soc-generator` / Sley (Python) reads the emitted JSONL against the published
  schema. See `soc-generator/docs/architecture/flow-log-root-cause-analysis.md`.

Apache-2.0.
