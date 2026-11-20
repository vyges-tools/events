//! The debug flags read from the real environment. ONE test in this binary, so no other test can
//! read the once-parsed variables before they are set (no race).

use vyges_events::debug::{debug_enabled, debug_specs, register_alias, warn_unknown_debug_groups, DebugGroup};
use vyges_events::logger::Logger;

#[test]
fn the_environment_flags_reach_every_new_logger() {
    let dir = std::env::temp_dir().join(format!("vyges-events-env-{}", std::process::id()));
    std::env::set_var("VYGES_DEBUG", "cts:tree=2,grt:*=1,bad-entry,grt:nope=1");
    std::env::set_var("VYGES_STAGE_TIMES", "1");
    std::env::set_var("VYGES_DEBUG_DIR", &dir);
    std::env::set_var("OLD_PDN_TRACE", "1");

    // The malformed entry is dropped (with one warning); the rest apply.
    assert_eq!(debug_specs().len(), 3);

    // Applied at construction; the engine's set_debug_level applies after it.
    let mut l = Logger::new();
    assert!(l.debug_on("CTS", "tree", 2) && !l.debug_on("CTS", "tree", 3));
    assert!(l.debug_on("GRT", "anything", 1));
    l.set_debug_level("CTS", "tree", 0);
    assert!(!l.debug_on("CTS", "tree", 1));
    // A logger that ignores the environment sees none of it.
    assert!(!Logger::without_env().debug_on("CTS", "tree", 1));

    // An old name maps onto a group, for loggers built after it and for code without one.
    assert!(!debug_enabled("pdn", "vy_trace", 1));
    assert!(register_alias("OLD_PDN_TRACE", "pdn", "vy_trace", 1));
    assert!(register_alias("OLD_PDN_TRACE", "pdn", "vy_trace", 1), "still set; the note is once");
    assert!(!register_alias("OLD_UNSET_SWITCH", "pdn", "vy_other", 1));
    assert!(debug_enabled("PDN", "vy_trace", 1));
    assert!(Logger::new().debug_on("pdn", "vy_trace", 1));

    // grt:nope is not in grt's table: one warning.
    let groups = [DebugGroup { group: "incr", max_level: 1, what: "x", reference: Some("GRT incr") }];
    assert_eq!(warn_unknown_debug_groups("grt", &groups), 1);

    assert!(vyges_events::stage("find_delays").is_on());
    assert_eq!(vyges_events::debug_dir("GRT"), Some(dir.join("grt")));
    assert!(dir.join("grt").is_dir());
}
