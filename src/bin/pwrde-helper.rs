//! CEF subprocess executable: Chromium's renderer, GPU, utility and plugin
//! processes all run this binary (the cef-rs `cefsimple` helper, verbatim in
//! shape). The main process points `browser_subprocess_path` at it — inside
//! `Pwrde Helper.app` in the bundle, or the same layout under
//! `target/<profile>/pwrde-cef/` in dev (see `src/cef_app.rs`) — and Chromium
//! derives the ` (GPU)` / ` (Renderer)` / ` (Plugin)` / ` (Alerts)` siblings
//! from that path.
//!
//! Everything is resolved relative to this executable, three levels up from
//! `<Name>.app/Contents/MacOS/`: the sandbox library and the framework both
//! live in `Chromium Embedded Framework.framework` beside the helper bundles.
//! Nothing here touches pwrde's own state; the process is Chromium's.

use cef::{api_hash, args::Args, execute_process, library_loader::LibraryLoader, sandbox::Sandbox, sys, App};

fn main() {
    let args = Args::new();
    // The sandbox must be up before the framework is loaded.
    let mut sandbox = Sandbox::new();
    sandbox.initialize(args.as_main_args());

    let Ok(exe) = std::env::current_exe() else {
        eprintln!("pwrde-helper: cannot resolve its own path");
        std::process::exit(1);
    };
    let loader = LibraryLoader::new(&exe, true);
    if !loader.load() {
        eprintln!("pwrde-helper: cannot load the Chromium Embedded Framework");
        std::process::exit(1);
    }
    // Pins the API version the bindings were generated for; must precede any
    // other CEF call.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let code = execute_process(Some(args.as_main_args()), None::<&mut App>, std::ptr::null_mut());
    // `process::exit` skips destructors: unload the framework and tear the
    // sandbox down in that order first.
    drop(loader);
    drop(sandbox);
    std::process::exit(code);
}
