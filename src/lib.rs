//! # vyges-events
//!
//! Canonical structured events for Vyges loom engines and tools — the shared
//! **producer** side of the logging/event contract (schema `vyges-events/1.0`).
//!
//! - The schema is **co-located and self-published**: [`EVENTS_SCHEMA`] embeds
//!   `schema/vyges-events-1.0.json`, and the `vyges-events --schema` binary dumps it
//!   (the events analog of a tool's `--describe`). Cross-language consumers (e.g. the
//!   Python root-cause pipeline) validate against that one crate-owned artifact.
//! - Emit is **daemonless**: each engine writes JSONL locally (default: stderr); the
//!   orchestrator (vyges-cli MCP `invoke`) aggregates per-run into the causal trail.
//! - With the default `tracing-bridge` feature, engines emit via standard
//!   `tracing::warn!(code = "DRC-0142", objects = ?objs, "spacing < min")` and get a
//!   canonical event for free.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// The schema version this crate implements.
pub const SCHEMA_VERSION: &str = "vyges-events/1.0";

/// The JSON Schema for a vyges event, embedded from `schema/vyges-events-1.0.json`.
/// This crate is the single home of the schema (co-located with the implementation).
pub const EVENTS_SCHEMA: &str = include_str!("../schema/vyges-events-1.0.json");

/// Return the embedded `vyges-events/1.0` JSON Schema text.
pub fn schema() -> &'static str {
    EVENTS_SCHEMA
}

/// Event severity (maps 1:1 to `tracing::Level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// One structured event (`vyges-events/1.0`). Serialized as one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Always `"vyges-events/1.0"`.
    pub schema: String,
    /// Unix epoch milliseconds.
    pub ts_ms: u64,
    /// Orchestration run id (set by the orchestrator / span context).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Flow stage (synthesis, floorplan, route, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Ordinal step within the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<u32>,
    /// Emitting tool/engine (vyges-drc, vyges-sta-si, openroad, …).
    pub tool: String,
    /// Severity.
    pub severity: Severity,
    /// Structured message code (DRC-0142, ODB-0220, …) — the clustering key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Message with variable parts masked ("net %s undriven").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_template: Option<String>,
    /// The full human-readable message.
    pub raw_msg: String,
    /// Design objects named by the message ("net:data[3]", "macro:sram0") — the
    /// cross-stage co-reference key used by root-cause analysis.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<String>,
    /// Source location (path:line) if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

impl Event {
    /// A new event stamped `now`, with only the required fields set.
    pub fn new(tool: impl Into<String>, severity: Severity, raw_msg: impl Into<String>) -> Self {
        Event {
            schema: SCHEMA_VERSION.to_string(),
            ts_ms: now_ms(),
            run_id: None,
            stage: None,
            step_index: None,
            tool: tool.into(),
            severity,
            code: None,
            msg_template: None,
            raw_msg: raw_msg.into(),
            objects: Vec::new(),
            file: None,
        }
    }

    /// Set the structured message code (builder).
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Set the design objects (builder).
    pub fn with_objects(mut self, objects: Vec<String>) -> Self {
        self.objects = objects;
        self
    }

    /// Serialize to one JSONL line.
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Emit an event as one JSONL line to stderr — the canonical local sink. The
/// orchestrator aggregates per-run (daemonless: no central logging service).
pub fn emit(event: &Event) {
    eprintln!("{}", event.to_jsonl());
}

/// A `tracing` bridge so engines emit via standard macros and get canonical JSONL.
#[cfg(feature = "tracing-bridge")]
pub mod tracing_bridge {
    use super::{emit, Event, Severity};
    use tracing::field::{Field, Visit};
    use tracing::{Event as TEvent, Level, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::registry::LookupSpan;

    #[derive(Default)]
    struct Fields {
        message: Option<String>,
        code: Option<String>,
        msg_template: Option<String>,
        objects: Vec<String>,
        file: Option<String>,
    }

    impl Visit for Fields {
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "message" => self.message = Some(value.to_string()),
                "code" => self.code = Some(value.to_string()),
                "msg_template" => self.msg_template = Some(value.to_string()),
                "file" => self.file = Some(value.to_string()),
                "objects" => {
                    self.objects = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                _ => {}
            }
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let s = format!("{:?}", value);
            match field.name() {
                "message" => self.message = Some(s),
                "code" => self.code = Some(s.trim_matches('"').to_string()),
                "msg_template" => self.msg_template = Some(s.trim_matches('"').to_string()),
                "file" => self.file = Some(s.trim_matches('"').to_string()),
                "objects" => {
                    self.objects = s
                        .trim_matches(|c| c == '[' || c == ']')
                        .split(',')
                        .map(|x| x.trim().trim_matches('"').to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                }
                _ => {}
            }
        }
    }

    /// A `tracing_subscriber::Layer` that serializes each event as `vyges-events/1.0` JSONL.
    pub struct VygesEventsLayer {
        tool: String,
    }

    impl VygesEventsLayer {
        pub fn new(tool: impl Into<String>) -> Self {
            Self { tool: tool.into() }
        }
    }

    impl<S> Layer<S> for VygesEventsLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, event: &TEvent<'_>, _ctx: Context<'_, S>) {
            let mut f = Fields::default();
            event.record(&mut f);
            let severity = match *event.metadata().level() {
                Level::ERROR => Severity::Error,
                Level::WARN => Severity::Warn,
                Level::INFO => Severity::Info,
                Level::DEBUG => Severity::Debug,
                Level::TRACE => Severity::Trace,
            };
            let mut ev = Event::new(self.tool.clone(), severity, f.message.unwrap_or_default());
            ev.code = f.code;
            ev.msg_template = f.msg_template;
            ev.objects = f.objects;
            ev.file = f
                .file
                .or_else(|| event.metadata().file().map(|s| s.to_string()));
            emit(&ev);
        }
    }

    /// Install the vyges-events JSONL layer as the global default subscriber.
    /// TODO: pull run_id/stage/step_index from span context (set by the orchestrator).
    pub fn init(tool: impl Into<String>) {
        use tracing_subscriber::prelude::*;
        let _ = tracing_subscriber::registry()
            .with(VygesEventsLayer::new(tool))
            .try_init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_roundtrips_jsonl() {
        let ev = Event::new("vyges-drc", Severity::Warn, "spacing < min on net data[3]")
            .with_code("DRC-0142")
            .with_objects(vec!["net:data[3]".into(), "macro:sram0".into()]);
        let line = ev.to_jsonl();
        let back: Event = serde_json::from_str(&line).unwrap();
        assert_eq!(back.schema, SCHEMA_VERSION);
        assert_eq!(back.code.as_deref(), Some("DRC-0142"));
        assert_eq!(back.objects.len(), 2);
        assert_eq!(back.severity, Severity::Warn);
    }

    #[test]
    fn schema_is_valid_json() {
        let v: serde_json::Value = serde_json::from_str(schema()).unwrap();
        assert_eq!(v["title"], "vyges-events/1.0");
    }
}
