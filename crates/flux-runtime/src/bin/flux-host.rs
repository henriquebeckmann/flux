//! Standalone Extension Host, used by the runtime's own integration tests. The launcher
//! normally runs `flux host`, which calls the same entry point.

fn main() {
    std::process::exit(flux_runtime::host::main());
}
