//! CEF subprocess entry point for web tabs.
//!
//! Chromium runs its renderer, GPU, utility and plugin work in child
//! processes. `scripts/make-app.sh` wraps this binary into the helper bundles
//! CEF launches for them (`Pwrde Helper.app` and its `(GPU)` / `(Renderer)` /
//! `(Plugin)` / `(Alerts)` variants under `Contents/Frameworks/`), so the main
//! `pwrde` binary is never re-launched as a child. All this does is start the
//! sandbox, load the framework from the enclosing bundle and hand the process
//! to `cef::execute_process`; see `src/webview_cef.rs` for the browser side.

use cef::args::Args;

fn main() {
    let args = Args::new();

    // The sandbox must be up before the framework loads; the guard has to
    // outlive `execute_process`.
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());

    let Ok(exe) = std::env::current_exe() else {
        eprintln!("pwrde-helper: cannot resolve its own path");
        std::process::exit(1);
    };
    let loader = cef::library_loader::LibraryLoader::new(&exe, true);
    if !loader.load() {
        eprintln!("pwrde-helper: cannot load the Chromium Embedded Framework");
        std::process::exit(1);
    }

    // Pin the API version the wrapper was compiled against.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let code = cef::execute_process(
        Some(args.as_main_args()),
        None::<&mut cef::App>,
        std::ptr::null_mut(),
    );
    drop(loader);
    drop(sandbox);
    std::process::exit(code);
}
