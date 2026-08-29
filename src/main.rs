//! Native macOS entry point. All the app lives in the `pwrde` library crate so
//! the wasm32 build under `web/` can boot the same modules.

fn main() {
    pwrde::app::run_native();
}
