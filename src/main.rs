//! `vyges-events` CLI — dumps the co-located `vyges-events/1.0` JSON Schema
//! (the events analog of a tool's `--describe`), so any consumer (incl. the
//! Python root-cause pipeline) can fetch the one crate-owned schema artifact.

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "--schema" => println!("{}", vyges_events::schema()),
        "--version" | "-V" => println!("vyges-events {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!(
                "vyges-events {} — canonical structured events for Vyges tools",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("usage: vyges-events --schema | --version");
        }
    }
}
