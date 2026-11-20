//! Debug groups, the dump directory and old-name aliases: the shared half of the debug-flag
//! standard (`VYGES_DEBUG`, `VYGES_DEBUG_DIR`).
//!
//! - `VYGES_DEBUG=<tool>:<group>=<level>[,...]` has the meaning of the reference's
//!   `set_debug_level <tool> <group> <level>`; `<group>` may be `*` (every group of that tool).
//!   It is parsed once and applied to every [`Logger`](crate::logger::Logger) when it is built,
//!   so an engine's own later `set_debug_level` still applies after it. A malformed entry gives
//!   one warning naming it; the rest still apply. It never panics. A group may contain spaces,
//!   as some reference groups do (`VYGES_DEBUG='cts:tech char=1'`).
//! - Tool names match case-insensitively (`grt` and `GRT` are one tool); group names are exact.
//! - An engine declares its groups in one [`DebugGroup`] table, lists them in `--describe`
//!   ([`describe_debug_groups`], [`render_debug_groups`]) and checks `VYGES_DEBUG` against it
//!   ([`warn_unknown_debug_groups`]): an unknown group is a warning naming the valid ones.
//! - `VYGES_DEBUG_DIR=<dir>`: large traces and dumps go to `<dir>/<tool>/` ([`debug_dir`]).
//! - An old environment variable maps onto a group with [`register_alias`], with a one-time
//!   deprecation note.

use crate::{log, Severity};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The tool name this crate's own warnings carry.
const TOOL: &str = "vyges-events";

/// The wildcard group: every group of a tool.
pub const ALL_GROUPS: &str = "*";

/// One `<tool>:<group>=<level>` entry of `VYGES_DEBUG`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSpec {
    pub tool: String,
    /// A group name, or [`ALL_GROUPS`].
    pub group: String,
    /// `0` removes the group, as in `set_debug_level`.
    pub level: i32,
}

/// A parsed `VYGES_DEBUG` value: the entries that parsed and the ones that did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDebug {
    pub specs: Vec<DebugSpec>,
    /// The malformed entries, as written.
    pub malformed: Vec<String>,
}

impl ParsedDebug {
    /// The one warning for the malformed entries, or `None` when there are none.
    pub fn warning(&self) -> Option<String> {
        if self.malformed.is_empty() {
            return None;
        }
        let list: Vec<String> = self.malformed.iter().map(|m| format!("'{m}'")).collect();
        Some(format!(
            "VYGES_DEBUG: ignoring malformed entr{} {} (expected <tool>:<group>=<level>, \
             group may be *, level a non-negative integer)",
            if list.len() == 1 { "y" } else { "ies" },
            list.join(", ")
        ))
    }
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

/// Parse a `VYGES_DEBUG` value. Entries are comma-separated; blanks are skipped. Never panics.
pub fn parse_debug_spec(value: &str) -> ParsedDebug {
    let mut out = ParsedDebug::default();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let parsed = entry.split_once(':').and_then(|(tool, rest)| {
            let (group, level) = rest.split_once('=')?;
            let (tool, group, level) = (tool.trim(), group.trim(), level.trim());
            let level: i32 = level.parse().ok().filter(|l| *l >= 0)?;
            // Reference groups may hold spaces ("tech char"); only the separators are refused.
            let group_ok = !group.is_empty() && !group.contains([',', '=', ':']);
            (valid_name(tool) && group_ok).then(|| DebugSpec { tool: tool.to_string(), group: group.to_string(), level })
        });
        match parsed {
            Some(spec) => out.specs.push(spec),
            None => out.malformed.push(entry.to_string()),
        }
    }
    out
}

/// The `VYGES_DEBUG` entries, parsed once per process. The first call warns once if any entry
/// is malformed.
pub fn debug_specs() -> &'static [DebugSpec] {
    static SPECS: OnceLock<Vec<DebugSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        let raw = std::env::var("VYGES_DEBUG").unwrap_or_default();
        let parsed = parse_debug_spec(&raw);
        if let Some(w) = parsed.warning() {
            log(TOOL, Severity::Warn, w);
        }
        parsed.specs
    })
}

/// Debug levels per tool and group: the table behind `set_debug_level` / `debug_on`.
///
/// A group's own level wins over the tool's `*` level; a level of 0 removes the entry (so
/// `set_debug_level grt * 0` removes the wildcard, not the groups set by name).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugLevels {
    levels: BTreeMap<(String, String), i32>,
}

impl DebugLevels {
    pub fn new() -> Self {
        Self::default()
    }

    /// `set_debug_level tool group level`; 0 removes the entry.
    pub fn set(&mut self, tool: &str, group: &str, level: i32) {
        let key = (tool.to_ascii_uppercase(), group.to_string());
        if level == 0 {
            self.levels.remove(&key);
        } else {
            self.levels.insert(key, level);
        }
    }

    /// Apply entries in order (later entries override earlier ones).
    pub fn apply(&mut self, specs: &[DebugSpec]) {
        for s in specs {
            self.set(&s.tool, &s.group, s.level);
        }
    }

    /// The level in force for a tool and group: its own, else the tool's `*`, else none.
    pub fn level(&self, tool: &str, group: &str) -> Option<i32> {
        let tool = tool.to_ascii_uppercase();
        self.levels
            .get(&(tool.clone(), group.to_string()))
            .or_else(|| self.levels.get(&(tool, ALL_GROUPS.to_string())))
            .copied()
    }

    /// Whether a debug line of `level` prints.
    pub fn on(&self, tool: &str, group: &str, level: i32) -> bool {
        self.level(tool, group).is_some_and(|l| level <= l)
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }
}

/// `VYGES_DEBUG` plus every alias registered so far, in that order: what a new
/// [`Logger`](crate::logger::Logger) starts from.
pub fn env_debug_specs() -> Vec<DebugSpec> {
    let mut specs = debug_specs().to_vec();
    specs.extend(alias_specs().lock().map(|a| a.clone()).unwrap_or_default());
    specs
}

/// Whether a debug group is on from the environment (`VYGES_DEBUG` and registered aliases), for
/// code that has no [`Logger`](crate::logger::Logger). Check it before building a message.
/// With nothing set it allocates nothing; otherwise it rebuilds the table, so in a hot loop
/// check once and keep the answer.
pub fn debug_enabled(tool: &str, group: &str, level: i32) -> bool {
    let no_aliases = alias_specs().lock().map(|a| a.is_empty()).unwrap_or(true);
    if debug_specs().is_empty() && no_aliases {
        return false;
    }
    let mut l = DebugLevels::new();
    l.apply(&env_debug_specs());
    l.on(tool, group, level)
}

// ---------------------------------------------------------------------------------------------
// Group tables and --describe

/// One debug group an engine offers: the row of its `DEBUG_GROUPS` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DebugGroup {
    /// The group name: the reference's group where one exists, else `vy_`-prefixed.
    pub group: &'static str,
    /// The highest level the group prints at.
    pub max_level: i32,
    /// What it prints.
    pub what: &'static str,
    /// The reference debug group it mirrors (`"GRT incr"`), if any.
    pub reference: Option<&'static str>,
}

/// The groups as JSON for a machine-readable `--describe`:
/// `{"env": "VYGES_DEBUG", "tool": .., "groups": [{group, max_level, what, reference}, ..]}`.
pub fn describe_debug_groups(tool: &str, groups: &[DebugGroup]) -> serde_json::Value {
    serde_json::json!({ "env": "VYGES_DEBUG", "tool": tool, "groups": groups })
}

/// The groups as a text table for a human `--describe`.
pub fn render_debug_groups(tool: &str, groups: &[DebugGroup]) -> String {
    let mut out = format!("Debug groups (VYGES_DEBUG={tool}:<group>=<level>, group may be *):\n");
    if groups.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }
    let w = groups.iter().map(|g| g.group.len()).max().unwrap_or(0).max("group".len());
    out.push_str(&format!("  {:<w$}  max  {:<14}  what\n", "group", "reference"));
    for g in groups {
        out.push_str(&format!("  {:<w$}  {:>3}  {:<14}  {}\n", g.group, g.max_level, g.reference.unwrap_or("-"), g.what));
    }
    out
}

/// The warnings for `specs` entries of `tool` (case-insensitive) that name a group not in
/// `groups`. Each names the valid groups. `*` is always valid.
pub fn check_debug_groups(tool: &str, groups: &[DebugGroup], specs: &[DebugSpec]) -> Vec<String> {
    let known: BTreeSet<&str> = groups.iter().map(|g| g.group).collect();
    let valid: Vec<&str> = groups.iter().map(|g| g.group).collect();
    let valid = if valid.is_empty() { "(none)".to_string() } else { valid.join(", ") };
    specs
        .iter()
        .filter(|s| s.tool.eq_ignore_ascii_case(tool) && s.group != ALL_GROUPS && !known.contains(s.group.as_str()))
        .map(|s| format!("VYGES_DEBUG: unknown debug group {}:{}; valid groups for {tool}: {valid}", s.tool, s.group))
        .collect()
}

/// Check `VYGES_DEBUG` against an engine's table and warn on each unknown group. Returns the
/// number of warnings.
pub fn warn_unknown_debug_groups(tool: &str, groups: &[DebugGroup]) -> usize {
    let warnings = check_debug_groups(tool, groups, debug_specs());
    for w in &warnings {
        log(TOOL, Severity::Warn, w.clone());
    }
    warnings.len()
}

// ---------------------------------------------------------------------------------------------
// VYGES_DEBUG_DIR

/// `<base>/<tool>/`, created if missing (`tool` lowercased, so `GRT` and `grt` share it).
pub fn debug_dir_under(base: &Path, tool: &str) -> std::io::Result<PathBuf> {
    let dir = base.join(tool.to_ascii_lowercase());
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `$VYGES_DEBUG_DIR/<tool>/`, created on demand; `None` when `VYGES_DEBUG_DIR` is unset or
/// empty, or when the directory cannot be created (one warning per call saying why).
pub fn debug_dir(tool: &str) -> Option<PathBuf> {
    static BASE: OnceLock<Option<PathBuf>> = OnceLock::new();
    let base = BASE.get_or_init(|| std::env::var_os("VYGES_DEBUG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from));
    let base = base.as_deref()?;
    match debug_dir_under(base, tool) {
        Ok(d) => Some(d),
        Err(e) => {
            log(TOOL, Severity::Warn, format!("VYGES_DEBUG_DIR: cannot create {}: {e}", base.join(tool).display()));
            None
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Old names

fn alias_specs() -> &'static Mutex<Vec<DebugSpec>> {
    static A: Mutex<Vec<DebugSpec>> = Mutex::new(Vec::new());
    &A
}

/// The spec and deprecation note for an old variable whose value is `value`: `None` when the
/// variable is unset, empty or `0`.
pub fn alias_spec(old_var: &str, value: Option<&str>, tool: &str, group: &str, level: i32) -> Option<(DebugSpec, String)> {
    let v = value?.trim();
    if v.is_empty() || v == "0" {
        return None;
    }
    let note = format!("{old_var} is deprecated; use VYGES_DEBUG={tool}:{group}={level}");
    Some((DebugSpec { tool: tool.to_string(), group: group.to_string(), level }, note))
}

/// Map an old environment variable onto a debug group. When `old_var` is set (not empty, not
/// `0`), the group is turned on for every [`Logger`](crate::logger::Logger) built afterwards and
/// for [`debug_enabled`], and a deprecation note is logged once per variable per process.
/// Returns whether the variable is set, so an engine can still read its value (a path, a net
/// name) where the old switch carried one. Register aliases at the start of `main`, before any
/// `Logger` is built.
pub fn register_alias(old_var: &str, tool: &str, group: &str, level: i32) -> bool {
    let value = std::env::var(old_var).ok();
    let Some((spec, note)) = alias_spec(old_var, value.as_deref(), tool, group, level) else {
        return false;
    };
    static NOTED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let first = NOTED.lock().map(|mut n| n.insert(old_var.to_string())).unwrap_or(false);
    if first {
        log(TOOL, Severity::Warn, note);
        if let Ok(mut a) = alias_specs().lock() {
            a.push(spec);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPS: &[DebugGroup] = &[
        DebugGroup { group: "incr", max_level: 2, what: "incremental reroute", reference: Some("GRT incr") },
        DebugGroup { group: "vy_cost", max_level: 1, what: "edge costs", reference: None },
    ];

    fn spec(tool: &str, group: &str, level: i32) -> DebugSpec {
        DebugSpec { tool: tool.into(), group: group.into(), level }
    }

    #[test]
    fn parses_entries_in_order() {
        let p = parse_debug_spec("grt:incr=1, sta:graph=2,,grt:*=3");
        assert_eq!(p.specs, [spec("grt", "incr", 1), spec("sta", "graph", 2), spec("grt", "*", 3)]);
        assert!(p.malformed.is_empty());
        assert_eq!(p.warning(), None);
        // A reference group with a space ("tech char") is one group.
        assert_eq!(parse_debug_spec("CTS:tech char=2").specs, [spec("CTS", "tech char", 2)]);
        assert_eq!(parse_debug_spec(""), ParsedDebug::default());
    }

    #[test]
    fn malformed_entries_give_one_warning_and_the_rest_apply() {
        let p = parse_debug_spec("grt:incr=1,grt,grt:incr,grt:incr=x,:g=1,grt:=1,grt:incr=-1,s ta:x=1,sta:x=2");
        assert_eq!(p.specs, [spec("grt", "incr", 1), spec("sta", "x", 2)]);
        assert_eq!(p.malformed.len(), 7);
        let w = p.warning().expect("malformed entries warn");
        assert!(w.contains("'grt:incr=x'") && w.contains("'grt'") && w.starts_with("VYGES_DEBUG"), "{w}");
        assert_eq!(w.lines().count(), 1, "one warning line");
    }

    #[test]
    fn wildcard_covers_every_group_and_a_named_group_wins() {
        let mut l = DebugLevels::new();
        l.apply(&[spec("grt", "*", 1), spec("grt", "incr", 3)]);
        assert!(l.on("GRT", "anything", 1));
        assert!(!l.on("GRT", "anything", 2));
        assert!(l.on("grt", "incr", 3));
        assert!(!l.on("sta", "incr", 1), "the wildcard is per tool");
        l.set("GRT", "*", 0);
        assert!(!l.on("grt", "anything", 1));
        assert!(l.on("grt", "incr", 1));
    }

    #[test]
    fn unknown_group_warns_naming_the_valid_ones() {
        let specs = [spec("GRT", "incr", 1), spec("grt", "nope", 1), spec("grt", "*", 1), spec("sta", "nope", 1)];
        let w = check_debug_groups("grt", GROUPS, &specs);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("grt:nope") && w[0].contains("incr, vy_cost"), "{}", w[0]);
    }

    #[test]
    fn describe_lists_every_group_with_its_reference() {
        let t = render_debug_groups("grt", GROUPS);
        assert!(t.contains("incr") && t.contains("GRT incr") && t.contains("vy_cost") && t.contains("edge costs"), "{t}");
        let j = describe_debug_groups("grt", GROUPS);
        assert_eq!(j["groups"][0]["reference"], "GRT incr");
        assert_eq!(j["groups"][1]["reference"], serde_json::Value::Null);
        assert_eq!(j["groups"][0]["max_level"], 2);
    }

    #[test]
    fn debug_dir_is_one_subdir_per_tool() {
        let base = std::env::temp_dir().join(format!("vyges-events-debug-dir-{}", std::process::id()));
        let d = debug_dir_under(&base, "GRT").expect("temp dir is writable");
        assert_eq!(d, base.join("grt"));
        assert!(d.is_dir());
    }

    #[test]
    fn an_alias_maps_a_set_variable_onto_a_group() {
        assert_eq!(alias_spec("PDN_TRACE", None, "pdn", "vy_trace", 1), None);
        assert_eq!(alias_spec("PDN_TRACE", Some(""), "pdn", "vy_trace", 1), None);
        assert_eq!(alias_spec("PDN_TRACE", Some("0"), "pdn", "vy_trace", 1), None);
        let (s, note) = alias_spec("PDN_TRACE", Some("1"), "pdn", "vy_trace", 2).expect("set");
        assert_eq!(s, spec("pdn", "vy_trace", 2));
        assert!(note.contains("PDN_TRACE is deprecated") && note.contains("VYGES_DEBUG=pdn:vy_trace=2"), "{note}");
    }
}
