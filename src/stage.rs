//! Per-stage time and memory: `VYGES_STAGE_TIMES=1`.
//!
//! ```no_run
//! let _s = vyges_events::stage::stage("find_delays");
//! // ... the stage ...
//! // on drop, one line: stage, wall, CPU, RSS at the end, peak RSS
//! ```
//!
//! The line goes through [`crate::emit`] (JSONL when not on a TTY) as an `info` event with code
//! `VYGES-STAGE` and the event's `stage` field set to the stage name. Its text:
//!
//! `stage=find_delays wall_s=1.234 cpu_s=2.345 rss_mib=120.5 hwm_mib=310.2`
//!
//! - `cpu_s` is the whole process's user + system CPU over the stage (`getrusage`), so it covers
//!   every thread and can exceed `wall_s`.
//! - `rss_mib` is the resident set at the end of the stage; `hwm_mib` is the process's peak
//!   resident set so far (Linux `VmRSS` / `VmHWM` from `/proc/self/status`). The peak is
//!   process-lifetime, not reset per stage: a stage whose `hwm_mib` is above the previous line's
//!   raised the peak.
//! - On macOS there is no current-RSS counter in `getrusage`: the line says `rss_mib=n/a` and
//!   takes `hwm_mib` from `ru_maxrss`, and names its source.
//!
//! Off by default. When off, [`stage`] returns before reading any clock or counter: no syscall.

use crate::{emit, Event, Severity};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The code the stage line carries.
pub const STAGE_CODE: &str = "VYGES-STAGE";

/// Whether `VYGES_STAGE_TIMES` is set (not empty, not `0`). Read once per process.
pub fn stage_times_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("VYGES_STAGE_TIMES").is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0"))
}

/// Memory at one point: what the platform gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem {
    /// Resident set now, KiB (`None` where the platform has no counter).
    pub rss_kib: Option<u64>,
    /// Peak resident set of the process so far, KiB.
    pub hwm_kib: Option<u64>,
    /// Where the numbers came from, when it is not `/proc/self/status`.
    pub note: Option<&'static str>,
}

impl Mem {
    /// Sample this process's memory.
    pub fn sample() -> Mem {
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let field = |name: &str| {
                status.lines().find_map(|l| l.strip_prefix(name)?.trim().strip_suffix("kB")?.trim().parse().ok())
            };
            Mem { rss_kib: field("VmRSS:"), hwm_kib: field("VmHWM:"), note: None }
        }
        #[cfg(target_os = "macos")]
        {
            // ru_maxrss is in bytes on macOS.
            let hwm = rusage().map(|r| r.ru_maxrss.max(0) as u64 / 1024);
            Mem { rss_kib: None, hwm_kib: hwm, note: Some("macOS: no current RSS; hwm from getrusage ru_maxrss") }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Mem { rss_kib: None, hwm_kib: None, note: Some("no memory counters on this platform") }
        }
    }
}

#[cfg(unix)]
fn rusage() -> Option<libc::rusage> {
    // SAFETY: `rusage` is a plain C struct of integers and `timeval`s; all-zero is a valid value.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `ru` is a live, exclusively borrowed `rusage` for the duration of the call, and
    // RUSAGE_SELF asks only for this process's counters; getrusage writes nothing else.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    (rc == 0).then_some(ru)
}

/// The process's user + system CPU time so far.
pub fn process_cpu() -> Option<Duration> {
    #[cfg(unix)]
    {
        let tv = |t: libc::timeval| Duration::from_secs(t.tv_sec.max(0) as u64) + Duration::from_micros(t.tv_usec.max(0) as u64);
        rusage().map(|r| tv(r.ru_utime) + tv(r.ru_stime))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn mib(kib: Option<u64>) -> String {
    kib.map_or_else(|| "n/a".to_string(), |k| format!("{:.1}", k as f64 / 1024.0))
}

/// The stage line's text.
pub fn stage_line(name: &str, wall: Duration, cpu: Option<Duration>, mem: &Mem) -> String {
    let cpu = cpu.map_or_else(|| "n/a".to_string(), |c| format!("{:.3}", c.as_secs_f64()));
    let mut s = format!(
        "stage={name} wall_s={:.3} cpu_s={cpu} rss_mib={} hwm_mib={}",
        wall.as_secs_f64(),
        mib(mem.rss_kib),
        mib(mem.hwm_kib)
    );
    if let Some(n) = mem.note {
        s.push_str(&format!(" ({n})"));
    }
    s
}

#[derive(Debug)]
struct Running {
    tool: String,
    name: String,
    wall0: Instant,
    cpu0: Option<Duration>,
}

/// A running stage; reports one line when dropped (when `VYGES_STAGE_TIMES` is on).
#[derive(Debug)]
#[must_use = "a stage reports when it is dropped; bind it: `let _s = stage(..)`"]
pub struct Stage {
    run: Option<Running>,
}

impl Stage {
    /// Whether this stage will report (the switch was on when it started).
    pub fn is_on(&self) -> bool {
        self.run.is_some()
    }

    /// A stage that never reports.
    pub fn off() -> Stage {
        Stage { run: None }
    }

    fn start(tool: &str, name: &str) -> Stage {
        Stage { run: Some(Running { tool: tool.to_string(), name: name.to_string(), wall0: Instant::now(), cpu0: process_cpu() }) }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let Some(r) = self.run.take() else { return };
        let wall = r.wall0.elapsed();
        let cpu = match (process_cpu(), r.cpu0) {
            (Some(now), Some(then)) => Some(now.saturating_sub(then)),
            _ => None,
        };
        let mut ev = Event::new(r.tool, Severity::Info, stage_line(&r.name, wall, cpu, &Mem::sample())).with_code(STAGE_CODE);
        ev.stage = Some(r.name);
        emit(&ev);
    }
}

/// Start a stage named after the reference function it reproduces. The event's tool is the
/// running binary's name. When `VYGES_STAGE_TIMES` is off this returns at once: no clock,
/// no counter, no allocation.
pub fn stage(name: &str) -> Stage {
    if !stage_times_on() {
        return Stage::off();
    }
    let tool = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "vyges".to_string());
    Stage::start(&tool, name)
}

/// [`stage`] with the event's tool named explicitly.
pub fn stage_in(tool: &str, name: &str) -> Stage {
    if !stage_times_on() {
        return Stage::off();
    }
    Stage::start(tool, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_line_has_every_field() {
        let mem = Mem { rss_kib: Some(2048), hwm_kib: Some(4096), note: None };
        let l = stage_line("find_delays", Duration::from_millis(1500), Some(Duration::from_millis(2250)), &mem);
        assert_eq!(l, "stage=find_delays wall_s=1.500 cpu_s=2.250 rss_mib=2.0 hwm_mib=4.0");
        let mac = Mem { rss_kib: None, hwm_kib: Some(1024), note: Some("macOS: no current RSS") };
        let l = stage_line("s", Duration::ZERO, None, &mac);
        assert_eq!(l, "stage=s wall_s=0.000 cpu_s=n/a rss_mib=n/a hwm_mib=1.0 (macOS: no current RSS)");
    }

    #[test]
    fn the_platform_counters_read() {
        let m = Mem::sample();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(m.hwm_kib.is_some_and(|k| k > 0), "{m:?}");
        #[cfg(target_os = "linux")]
        assert!(m.rss_kib.is_some_and(|k| k > 0) && m.note.is_none(), "{m:?}");
        #[cfg(target_os = "macos")]
        assert!(m.rss_kib.is_none() && m.note.is_some_and(|n| n.contains("macOS")), "{m:?}");
        #[cfg(unix)]
        assert!(process_cpu().is_some());
    }

    #[test]
    fn an_off_stage_holds_nothing_and_reports_nothing() {
        // `stage` returns `Stage::off()` before any clock or counter when the switch is off; an
        // off stage has no start sample and its drop returns at once.
        let s = Stage::off();
        assert!(!s.is_on());
        drop(s);
        if !stage_times_on() {
            assert!(!stage("x").is_on());
            assert!(!stage_in("t", "x").is_on());
        }
    }

    #[test]
    fn an_on_stage_carries_its_start_sample() {
        let s = Stage::start("vyges-test", "find_delays");
        assert!(s.is_on());
        let r = s.run.as_ref().expect("on");
        assert_eq!((r.tool.as_str(), r.name.as_str()), ("vyges-test", "find_delays"));
        #[cfg(unix)]
        assert!(r.cpu0.is_some());
    }
}
