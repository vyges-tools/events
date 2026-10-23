//! A logger that writes each message line exactly as the reference flow's logger does —
//! `[INFO GRT-0001] text`, `[WARNING RSZ-0062] text`, `[DEBUG CTS-tree] text` — and keeps the
//! same bookkeeping: a per-code print limit, suppression, debug groups and warning/error
//! counts. Every line can also go out as a `vyges-events/1.0` [`Event`], so an engine's
//! scored text log and its causal trail are one path.
//!
//! The rules, each pinned by a test below:
//! - A message code is a tool and an id (`0..=9999`), shown as `TOOL-NNNN`.
//! - Each code prints at most [`MAX_MESSAGE_PRINT`] times. The print after that is replaced by
//!   one `message limit (1000) reached. This message will no longer print.` line at the same
//!   level, and the code is silent from then on. Its count saturates one past the limit.
//! - [`Logger::suppress`] silences a code without the limit line; [`Logger::unsuppress`] resets
//!   its count to zero, so it prints again (and can reach the limit again).
//! - A warning or an error is counted even when it does not print (limited or suppressed).
//! - A debug line prints when its tool+group level is at least the line's level; a level of 0
//!   removes the group. Debug lines have no limit and are not counted.
//! - A report is the text alone: no prefix, no limit, no count.
//! - [`Logger::summary`] lists, per code that was last logged as a warning and has a nonzero
//!   count, that count — in tool-then-id order — plus the totals.

use crate::{emit, Event, Severity};
use std::collections::BTreeMap;

/// How many times one code prints before its limit line.
pub const MAX_MESSAGE_PRINT: u32 = 1000;

/// The largest message id a code can carry.
pub const MAX_MESSAGE_ID: u16 = 9999;

/// A message's level, as the reference's logger names them in a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

impl Level {
    /// The word inside the brackets.
    pub fn name(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
            Level::Critical => "CRITICAL",
        }
    }

    /// The event severity for this level (critical is an error that ends the run).
    pub fn severity(self) -> Severity {
        match self {
            Level::Debug => Severity::Debug,
            Level::Info => Severity::Info,
            Level::Warning => Severity::Warn,
            Level::Error | Level::Critical => Severity::Error,
        }
    }
}

/// `TOOL-NNNN`: the code a message line and its event carry.
pub fn code(tool: &str, id: u16) -> String {
    format!("{tool}-{id:04}")
}

/// Split a `TOOL-NNNN` code into its tool and id: three or more uppercase ASCII letters, a dash,
/// exactly four digits. `None` for anything else.
pub fn parse_code(code: &str) -> Option<(&str, u16)> {
    let (tool, id) = code.rsplit_once('-')?;
    let tool_ok = tool.len() >= 3 && tool.bytes().all(|b| b.is_ascii_uppercase());
    let id_ok = id.len() == 4 && id.bytes().all(|b| b.is_ascii_digit());
    if !(tool_ok && id_ok) {
        return None;
    }
    Some((tool, id.parse().ok()?))
}

/// A run stopped by an error message: the code it stopped on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub tool: String,
    pub id: u16,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&code(&self.tool, self.id))
    }
}

impl std::error::Error for Stopped {}

/// The end-of-run counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub warnings: u32,
    pub errors: u32,
    /// `(code, count)` per code last logged as a warning with a nonzero count, tool then id.
    pub warning_codes: Vec<(String, u32)>,
}

impl Summary {
    /// The counts as `(name, value)` metric pairs, in the order the reference's metrics file
    /// lists them: totals, then each code, then the number of distinct codes.
    pub fn metrics(&self) -> Vec<(String, u32)> {
        let mut m = vec![("flow__warnings__count".to_string(), self.warnings), ("flow__errors__count".to_string(), self.errors)];
        m.extend(self.warning_codes.iter().map(|(c, n)| (format!("flow__warnings__count:{c}"), *n)));
        m.push(("flow__warnings__type_count".to_string(), self.warning_codes.len() as u32));
        m
    }
}

#[derive(Debug, Clone, Copy)]
struct Counter {
    count: u32,
    /// The level the code was last logged at; `None` until it is logged once.
    level: Option<Level>,
}

/// The reference-format logger. Lines are kept in order ([`Logger::lines`]); with
/// [`Logger::echo`] each is also printed to stdout, and with [`Logger::events`] each message
/// is also emitted as an event.
#[derive(Debug, Default)]
pub struct Logger {
    lines: Vec<String>,
    counters: BTreeMap<(String, u16), Counter>,
    debug: BTreeMap<(String, String), i32>,
    warnings: u32,
    errors: u32,
    echo: bool,
    events: Option<String>,
}

impl Logger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also print each line to stdout as it is written.
    pub fn echo(mut self, on: bool) -> Self {
        self.echo = on;
        self
    }

    /// Also emit each message as a `vyges-events/1.0` event from `tool` (the engine's name).
    pub fn events(mut self, tool: impl Into<String>) -> Self {
        self.events = Some(tool.into());
        self
    }

    /// Every line written so far, in order.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Take the lines written so far, leaving none (a capture between two points of a run).
    pub fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    pub fn warning_count(&self) -> u32 {
        self.warnings
    }

    pub fn error_count(&self) -> u32 {
        self.errors
    }

    fn write(&mut self, text: &str) {
        // A message with newlines is printed as it is: one output line per text line.
        for line in text.split('\n') {
            if self.echo {
                println!("{line}");
            }
            self.lines.push(line.to_string());
        }
    }

    fn event(&self, level: Level, code: Option<String>, text: &str) {
        if let Some(tool) = &self.events {
            let mut ev = Event::new(tool.clone(), level.severity(), text);
            ev.code = code;
            emit(&ev);
        }
    }

    fn message(&mut self, level: Level, tool: &str, id: u16, text: &str) {
        assert!(id <= MAX_MESSAGE_ID, "message id {id} is over {MAX_MESSAGE_ID}");
        let c = self.counters.entry((tool.to_string(), id)).or_insert(Counter { count: 0, level: None });
        c.level = Some(level);
        let count = c.count;
        // Past the limit the count stays one over it (it neither prints nor grows).
        if count <= MAX_MESSAGE_PRINT {
            c.count += 1;
        }
        let tag = format!("[{} {}]", level.name(), code(tool, id));
        if count < MAX_MESSAGE_PRINT {
            self.write(&format!("{tag} {text}"));
            self.event(level, Some(code(tool, id)), text);
        } else if count == MAX_MESSAGE_PRINT {
            let limit = format!("message limit ({MAX_MESSAGE_PRINT}) reached. This message will no longer print.");
            self.write(&format!("{tag} {limit}"));
            self.event(level, Some(code(tool, id)), &limit);
        }
    }

    pub fn info(&mut self, tool: &str, id: u16, text: impl AsRef<str>) {
        self.message(Level::Info, tool, id, text.as_ref());
    }

    /// A warning: counted whether or not it prints.
    pub fn warn(&mut self, tool: &str, id: u16, text: impl AsRef<str>) {
        self.warnings += 1;
        self.message(Level::Warning, tool, id, text.as_ref());
    }

    /// An error: counted, written (subject to the limit like any message), and the run stops
    /// with its code.
    pub fn error<T>(&mut self, tool: &str, id: u16, text: impl AsRef<str>) -> Result<T, Stopped> {
        self.errors += 1;
        self.message(Level::Error, tool, id, text.as_ref());
        Err(Stopped { tool: tool.to_string(), id })
    }

    /// A critical message: written, NOT counted as an error, and the run stops with its code.
    pub fn critical<T>(&mut self, tool: &str, id: u16, text: impl AsRef<str>) -> Result<T, Stopped> {
        self.message(Level::Critical, tool, id, text.as_ref());
        Err(Stopped { tool: tool.to_string(), id })
    }

    /// The text alone: no prefix, no limit, no count.
    pub fn report(&mut self, text: impl AsRef<str>) {
        self.write(text.as_ref());
        self.event(Level::Info, None, text.as_ref());
    }

    /// Silence a code: no further lines, not even the limit line. Warnings stay counted.
    pub fn suppress(&mut self, tool: &str, id: u16) {
        self.counters.entry((tool.to_string(), id)).or_insert(Counter { count: 0, level: None }).count = MAX_MESSAGE_PRINT + 1;
    }

    /// Let a code print again, from a count of zero.
    pub fn unsuppress(&mut self, tool: &str, id: u16) {
        self.counters.entry((tool.to_string(), id)).or_insert(Counter { count: 0, level: None }).count = 0;
    }

    /// `set_debug_level tool group level`; a level of 0 removes the group.
    pub fn set_debug_level(&mut self, tool: &str, group: &str, level: i32) {
        let key = (tool.to_string(), group.to_string());
        if level == 0 {
            self.debug.remove(&key);
        } else {
            self.debug.insert(key, level);
        }
    }

    /// Whether a debug line of `level` for this tool and group prints. Check it before building
    /// an expensive message, or use [`Logger::debug`], which only builds it when it prints.
    pub fn debug_on(&self, tool: &str, group: &str, level: i32) -> bool {
        self.debug.get(&(tool.to_string(), group.to_string())).is_some_and(|&l| level <= l)
    }

    /// `[DEBUG TOOL-group] text`, when the group's level is at least `level`.
    pub fn debug(&mut self, tool: &str, group: &str, level: i32, text: impl FnOnce() -> String) {
        if self.debug_on(tool, group, level) {
            let t = text();
            self.write(&format!("[{} {tool}-{group}] {t}", Level::Debug.name()));
            self.event(Level::Debug, None, &t);
        }
    }

    /// The counts so far.
    pub fn summary(&self) -> Summary {
        let warning_codes = self
            .counters
            .iter()
            .filter(|(_, c)| c.count > 0 && c.level == Some(Level::Warning))
            .map(|((t, id), c)| (code(t, *id), c.count))
            .collect();
        Summary { warnings: self.warnings, errors: self.errors, warning_codes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_level_tool_and_four_digit_id() {
        let mut l = Logger::new();
        l.info("GRT", 1, "Min routing layer: met1");
        l.warn("RSZ", 62, "Unable to repair");
        l.report("plain text");
        assert_eq!(l.lines(), ["[INFO GRT-0001] Min routing layer: met1", "[WARNING RSZ-0062] Unable to repair", "plain text"]);
    }

    #[test]
    fn a_message_with_newlines_is_one_line_per_text_line() {
        let mut l = Logger::new();
        l.info("CTS", 5, "a\nb");
        assert_eq!(l.lines(), ["[INFO CTS-0005] a", "b"]);
    }

    #[test]
    fn a_code_prints_up_to_the_limit_then_one_limit_line_then_nothing() {
        let mut l = Logger::new();
        for _ in 0..MAX_MESSAGE_PRINT + 5 {
            l.warn("ODB", 92, "x");
        }
        assert_eq!(l.lines().len(), MAX_MESSAGE_PRINT as usize + 1);
        assert_eq!(l.lines()[MAX_MESSAGE_PRINT as usize - 1], "[WARNING ODB-0092] x");
        assert_eq!(l.lines()[MAX_MESSAGE_PRINT as usize], "[WARNING ODB-0092] message limit (1000) reached. This message will no longer print.");
        // Every warning counts; the code's own count saturates one past the limit.
        assert_eq!(l.warning_count(), MAX_MESSAGE_PRINT + 5);
        assert_eq!(l.summary().warning_codes, [("ODB-0092".to_string(), MAX_MESSAGE_PRINT + 1)]);
    }

    #[test]
    fn the_limit_is_per_code() {
        let mut l = Logger::new();
        for _ in 0..MAX_MESSAGE_PRINT {
            l.info("DPL", 1, "a");
        }
        l.info("DPL", 2, "b");
        l.info("DPL", 1, "a");
        let n = MAX_MESSAGE_PRINT as usize;
        assert_eq!(l.lines()[n], "[INFO DPL-0002] b");
        assert!(l.lines()[n + 1].starts_with("[INFO DPL-0001] message limit"));
    }

    #[test]
    fn suppress_silences_without_the_limit_line_and_still_counts() {
        let mut l = Logger::new();
        l.suppress("GRT", 97);
        l.warn("GRT", 97, "x");
        l.warn("GRT", 97, "x");
        assert!(l.lines().is_empty());
        assert_eq!(l.warning_count(), 2);
        l.unsuppress("GRT", 97);
        l.warn("GRT", 97, "y");
        assert_eq!(l.lines(), ["[WARNING GRT-0097] y"]);
        assert_eq!(l.summary().warning_codes, [("GRT-0097".to_string(), 1)]);
    }

    #[test]
    fn a_warning_code_unsuppressed_back_to_zero_is_not_in_the_summary() {
        let mut l = Logger::new();
        l.warn("GRT", 97, "x");
        l.unsuppress("GRT", 97);
        assert!(l.summary().warning_codes.is_empty());
        assert_eq!(l.warning_count(), 1);
    }

    #[test]
    fn a_suppressed_code_never_logged_is_not_in_the_summary() {
        let mut l = Logger::new();
        l.suppress("GRT", 97);
        assert!(l.summary().warning_codes.is_empty());
    }

    #[test]
    fn the_summary_lists_codes_last_logged_as_warnings_in_tool_then_id_order() {
        let mut l = Logger::new();
        l.warn("RSZ", 2, "b");
        l.warn("GRT", 9, "a");
        l.warn("GRT", 3, "a");
        l.warn("DPL", 4, "c");
        l.info("DPL", 4, "c");
        let s = l.summary();
        assert_eq!(s.warning_codes, [("GRT-0003".to_string(), 1), ("GRT-0009".to_string(), 1), ("RSZ-0002".to_string(), 1)]);
        assert_eq!(s.warnings, 4);
        let m = s.metrics();
        assert_eq!(m.first().unwrap(), &("flow__warnings__count".to_string(), 4));
        assert_eq!(m[2], ("flow__warnings__count:GRT-0003".to_string(), 1));
        assert_eq!(m.last().unwrap(), &("flow__warnings__type_count".to_string(), 3));
    }

    #[test]
    fn error_counts_writes_and_stops_critical_does_not_count() {
        let mut l = Logger::new();
        let e: Result<(), _> = l.error("PDN", 175, "no grid");
        assert_eq!(e.unwrap_err().to_string(), "PDN-0175");
        let c: Result<(), _> = l.critical("ODB", 1, "fatal");
        assert!(c.is_err());
        assert_eq!(l.error_count(), 1);
        assert_eq!(l.lines(), ["[ERROR PDN-0175] no grid", "[CRITICAL ODB-0001] fatal"]);
    }

    #[test]
    fn debug_prints_at_or_below_the_group_level_and_zero_removes_it() {
        let mut l = Logger::new();
        let mut built = 0;
        l.debug("CTS", "tree", 1, || {
            built += 1;
            "off".into()
        });
        assert_eq!(built, 0, "a debug message is not built when it does not print");
        l.set_debug_level("CTS", "tree", 2);
        l.debug("CTS", "tree", 2, || "two".into());
        l.debug("CTS", "tree", 3, || "three".into());
        l.debug("CTS", "other", 1, || "other".into());
        l.set_debug_level("CTS", "tree", 0);
        l.debug("CTS", "tree", 1, || "gone".into());
        assert!(!l.debug_on("CTS", "tree", 0), "a removed group prints nothing, at any level");
        assert_eq!(l.lines(), ["[DEBUG CTS-tree] two"]);
        assert_eq!(l.warning_count(), 0);
    }

    #[test]
    fn take_lines_captures_between_two_points() {
        let mut l = Logger::new();
        l.info("TAP", 1, "a");
        assert_eq!(l.take_lines(), ["[INFO TAP-0001] a"]);
        l.info("TAP", 2, "b");
        assert_eq!(l.lines(), ["[INFO TAP-0002] b"]);
    }

    #[test]
    fn codes_parse_only_in_tool_dash_four_digit_shape() {
        assert_eq!(parse_code("GRT-0097"), Some(("GRT", 97)));
        assert_eq!(code("RSZ", 7), "RSZ-0007");
        for bad in ["DRC-WIDTH", "grt-0097", "GR-0001", "GRT-97", "GRT-00097", "GRT0097"] {
            assert_eq!(parse_code(bad), None, "{bad}");
        }
    }

    #[test]
    #[should_panic(expected = "over 9999")]
    fn an_id_over_the_maximum_is_refused() {
        Logger::new().info("GRT", 10000, "x");
    }
}
