//! Main-process lifecycle of the Chromium Embedded Framework (CEF), the
//! engine behind webview tabs (`webview.rs` owns the browsers themselves).
//!
//! gpui owns the `NSApplication` and its run loop, so CEF is a guest here:
//!
//! - [`init`] runs once, on the main thread, after gpui has built its
//!   application object and before any browser exists. It teaches gpui's
//!   `NSApplication` class the `CefAppProtocol` methods Chromium requires
//!   (`isHandlingSendEvent` / `setHandlingSendEvent:`, added at runtime with
//!   `class_addMethod`, plus a `sendEvent:` wrapper that keeps the flag set
//!   while an event is dispatched — what `CefScopedSendingEvent` does in a
//!   stock CEF app), loads the framework, and calls `cef::initialize` with
//!   `external_message_pump` so Chromium never runs a loop of its own.
//! - [`pump`] is `do_message_loop_work`, called from the 16ms pump in
//!   `main.rs` and nowhere else: on macOS it turns the native run loop, which
//!   runs whatever gpui has queued on the main thread, so calling it while a
//!   gpui entity is borrowed (any event handler, any `update`) panics gpui
//!   with "RefCell already borrowed". It also refuses to nest.
//! - [`shutdown`] waits (bounded) for the browsers `webview::Manager` closed
//!   to finish closing, then calls `cef::shutdown`. For the same reason
//!   `Action::Quit` only calls [`request_quit`] and the 16ms pump — outside
//!   any borrow — does the shutdown and exits; gpui's last-window-closed quit
//!   runs its observers with the app borrowed, so that path calls
//!   [`shutdown_now`], which skips the wait.
//!
//! The framework and the subprocess helper (`src/bin/pwrde-helper.rs`) are
//! found by the pure [`locate`]: inside `Pwrde.app` they are
//! `Contents/Frameworks/Chromium Embedded Framework.framework` and
//! `Contents/Frameworks/Pwrde Helper.app` (assembled by `scripts/make-app.sh`);
//! for a bare `cargo run` binary the CEF distribution is the one the build
//! used (`CEF_PATH`, see CLAUDE.md) and [`prepare_dev_layout`] lays out the
//! same shape under `target/<profile>/pwrde-cef/Pwrde.app` — an `Info.plist`,
//! a copy of the framework (an APFS clone) and the five `Pwrde Helper*.app`
//! bundles Chromium derives from the helper path, each holding a copy of
//! `target/<profile>/pwrde-helper` — and hands it to CEF as
//! `main_bundle_path`, so webview tabs work without building an .app. The
//! stand-in bundle is not optional: browser and helpers find each other
//! through a Mach service named after the outer bundle's identifier, which
//! each side reads from the bundle it believes it is in.
//!
//! Chromium's profile (cookies, cache, storage) lives in
//! `<data_dir>/pwrde/cef`, worktree-scoped like `state.db`; Chromium holds a
//! process lock on it, so a second pwrde on the same scope runs without
//! webviews. Cookies are encrypted with Chromium's mock keychain unless the
//! `webview.keychain` setting is on: the real "Chromium Safe Storage" item
//! prompts for access again after every ad-hoc re-sign.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

pub const FRAMEWORK_DIR: &str = "Chromium Embedded Framework.framework";
const FRAMEWORK_BIN: &str = "Chromium Embedded Framework";
/// The helper bundle's base name; Chromium appends the suffixes below to it.
const HELPER_NAME: &str = "Pwrde Helper";
/// The cargo bin every helper bundle's executable is a copy of.
const HELPER_BIN: &str = "pwrde-helper";
/// One helper bundle per Chromium process flavour, derived by Chromium from
/// `browser_subprocess_path` (`<name><suffix>.app/Contents/MacOS/<name><suffix>`).
/// `scripts/make-app.sh` bundles the same five.
const HELPER_SUFFIXES: [&str; 5] = ["", " (GPU)", " (Renderer)", " (Plugin)", " (Alerts)"];
/// The dev layout beside the cargo binaries: a stand-in main bundle.
const DEV_DIR: &str = "pwrde-cef";
const DEV_BUNDLE: &str = "Pwrde.app";
/// `scripts/make-app.sh`'s `BUNDLE_ID`; the helpers hang off it.
const BUNDLE_ID: &str = "com.pwrde.terminal";

/// How long [`shutdown`] waits for closing browsers: steps × interval.
const SHUTDOWN_STEPS: usize = 100;
const SHUTDOWN_STEP: Duration = Duration::from_millis(5);

/// `Ok` once `cef::initialize` succeeded; the error is what a failed webview
/// creation reports. Unset until [`init`] has run.
static STATE: OnceLock<Result<(), String>> = OnceLock::new();
static SHUT_DOWN: AtomicBool = AtomicBool::new(false);
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Browsers created and not yet through `on_before_close`.
static BROWSERS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static PUMPING: Cell<bool> = const { Cell::new(false) };
}

/// Where the framework and the helper are, and — for a bare cargo binary —
/// what [`prepare_dev_layout`] has to assemble first.
#[derive(Clone, Debug, PartialEq)]
struct Paths {
    /// `…/Chromium Embedded Framework.framework` (CEF's `framework_dir_path`).
    framework: PathBuf,
    /// The base helper executable (CEF's `browser_subprocess_path`).
    helper: PathBuf,
    /// `Pwrde.app` — the real bundle, or the dev stand-in (CEF's
    /// `main_bundle_path`).
    main_bundle: PathBuf,
    dev: Option<DevLayout>,
}

#[derive(Clone, Debug, PartialEq)]
struct DevLayout {
    /// `target/<profile>/pwrde-cef/Pwrde.app`.
    bundle: PathBuf,
    /// The distribution's framework the layout copies.
    framework_src: PathBuf,
    /// `target/<profile>/pwrde-helper`.
    helper_src: PathBuf,
}

/// The helper executable for `suffix` under `dir` (the bundle's
/// `Contents/Frameworks`, or the dev layout).
fn helper_exe(dir: &Path, suffix: &str) -> PathBuf {
    let name = format!("{HELPER_NAME}{suffix}");
    dir.join(format!("{name}.app")).join("Contents").join("MacOS").join(name)
}

/// Resolve the framework and helper for the executable at `exe`. Pure over
/// `exists`, so tests pin both layouts. A bundle (`…/Contents/MacOS/<exe>`
/// with the framework in `Contents/Frameworks`) wins; otherwise `cef_dir` —
/// the CEF binary distribution the build used — backs the dev layout beside
/// the executable, which also needs the `pwrde-helper` cargo bin built.
fn locate(exe: &Path, cef_dir: Option<&Path>, exists: &dyn Fn(&Path) -> bool) -> Result<Paths, String> {
    let exe_dir = exe.parent().ok_or("cannot resolve the executable's directory")?;
    let contents = exe_dir.parent().filter(|contents| {
        exe_dir.file_name().is_some_and(|name| name == "MacOS")
            && contents.file_name().is_some_and(|name| name == "Contents")
    });
    if let Some(contents) = contents {
        let frameworks = contents.join("Frameworks");
        let framework = frameworks.join(FRAMEWORK_DIR);
        if let (true, Some(bundle)) = (exists(&framework), contents.parent()) {
            return Ok(Paths {
                framework,
                helper: helper_exe(&frameworks, ""),
                main_bundle: bundle.to_path_buf(),
                dev: None,
            });
        }
    }
    let cef_dir = cef_dir.ok_or(
        "the CEF distribution was not found (set CEF_PATH to the directory the build used)",
    )?;
    let framework_src = cef_dir.join(FRAMEWORK_DIR);
    if !exists(&framework_src) {
        return Err(format!("no {FRAMEWORK_DIR} in {}", cef_dir.display()));
    }
    let helper_src = exe_dir.join(HELPER_BIN);
    if !exists(&helper_src) {
        return Err(format!(
            "{} is not built (run `cargo build`, which builds every bin)",
            helper_src.display()
        ));
    }
    let bundle = exe_dir.join(DEV_DIR).join(DEV_BUNDLE);
    let frameworks = bundle.join("Contents").join("Frameworks");
    Ok(Paths {
        framework: frameworks.join(FRAMEWORK_DIR),
        helper: helper_exe(&frameworks, ""),
        main_bundle: bundle.clone(),
        dev: Some(DevLayout { bundle, framework_src, helper_src }),
    })
}

/// The CEF distribution directory under a `CEF_PATH`-style `root`: cef-rs's
/// versioned layout `<root>/<version>/cef_macos_<arch>` (what its build
/// script downloads into), else `root` itself (an `export-cef-dir` export).
/// `version` is the build's `CEF_VERSION`, whose `+…` suffix is dropped.
fn cef_dir_in(root: &Path, version: &str, arch: &str, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let version = version.split('+').next().unwrap_or(version);
    let versioned = root.join(version).join(format!("cef_macos_{arch}"));
    [versioned, root.to_path_buf()].into_iter().find(|dir| exists(&dir.join(FRAMEWORK_DIR)))
}

/// The distribution this binary was built against: cef-rs's own lookup
/// (`CEF_PATH` in the environment, else the build's download directory),
/// then the `CEF_PATH` the build saw, for a launch whose environment lacks it.
fn cef_dir() -> Option<PathBuf> {
    let exists = |path: &Path| path.exists();
    cef::sys::get_cef_dir().filter(|dir| exists(&dir.join(FRAMEWORK_DIR))).or_else(|| {
        let version = std::ffi::CStr::from_bytes_with_nul(cef::sys::CEF_VERSION).ok()?.to_str().ok()?;
        cef_dir_in(Path::new(option_env!("CEF_PATH")?), version, std::env::consts::ARCH, &exists)
    })
}

/// Chromium's profile directory under `data_dir`: beside `state.db`, so it
/// is worktree-scoped the same way.
fn cache_dir_in(data_dir: &Path, scope: Option<&str>) -> PathBuf {
    crate::persist::db_path_in(data_dir, scope).with_file_name("cef")
}

/// A helper bundle's identifier: `.helper`, `.helper.gpu`, `.helper.renderer`, …
fn helper_bundle_id(suffix: &str) -> String {
    let flavour: String =
        suffix.chars().filter(|c| c.is_ascii_alphabetic()).flat_map(char::to_lowercase).collect();
    if flavour.is_empty() {
        format!("{BUNDLE_ID}.helper")
    } else {
        format!("{BUNDLE_ID}.helper.{flavour}")
    }
}

/// A dev-layout `Info.plist`: the stand-in main bundle's (`helper` false) or
/// a helper bundle's, where `LSUIElement` keeps the process out of the Dock.
/// `scripts/make-app.sh` writes the real bundle's.
fn info_plist(name: &str, executable: &str, id: &str, helper: bool) -> String {
    let ui_element = if helper { "\t<key>LSUIElement</key>\n\t<string>1</string>\n" } else { "" };
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundleDisplayName</key>
	<string>{name}</string>
	<key>CFBundleExecutable</key>
	<string>{executable}</string>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
{ui_element}	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
"#
    )
}

/// Whether `copy` is still an up-to-date copy of `source`: the same size and
/// not older.
fn is_fresh(copy: &Path, source: &Path) -> bool {
    let (Ok(copy), Ok(source)) = (std::fs::metadata(copy), std::fs::metadata(source)) else {
        return false;
    };
    copy.len() == source.len()
        && matches!((copy.modified(), source.modified()), (Ok(copy), Ok(source)) if copy >= source)
}

/// Copy a directory tree. `std::fs::copy` clones on APFS, so the framework's
/// ~300 MB cost no disk or time there.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Assemble (or refresh) the dev layout: a copy of the framework and the five
/// helper bundles. The framework has to be a real copy — Chromium's sandbox
/// lets a helper read only inside the directories the browser process names,
/// and a symlink's target is outside them. Copies are refreshed only when the
/// source is newer or differs in size, and a helper executable always lands
/// on a fresh inode — macOS caches code signatures per file and kills a
/// binary rewritten in place.
fn prepare_dev_layout(dev: &DevLayout) -> Result<(), String> {
    let io = |what: &str, error: std::io::Error| format!("{what}: {error}");
    let contents = dev.bundle.join("Contents");
    let frameworks = contents.join("Frameworks");
    std::fs::create_dir_all(&frameworks).map_err(|e| io("create the CEF dev bundle", e))?;
    let write_plist = |path: PathBuf, plist: String| {
        if std::fs::read_to_string(&path).ok().as_deref() == Some(plist.as_str()) {
            return Ok(());
        }
        std::fs::write(&path, plist).map_err(|e| io("write a dev Info.plist", e))
    };
    write_plist(contents.join("Info.plist"), info_plist("Pwrde", "pwrde", BUNDLE_ID, false))?;
    let framework = frameworks.join(FRAMEWORK_DIR);
    if framework.is_symlink()
        || !is_fresh(&framework.join(FRAMEWORK_BIN), &dev.framework_src.join(FRAMEWORK_BIN))
    {
        // Whatever is there: a stale copy, or a link from an older layout.
        if framework.is_symlink() {
            let _ = std::fs::remove_file(&framework);
        } else {
            let _ = std::fs::remove_dir_all(&framework);
        }
        copy_tree(&dev.framework_src, &framework).map_err(|e| io("copy the CEF framework", e))?;
    }
    for suffix in HELPER_SUFFIXES {
        let exe = helper_exe(&frameworks, suffix);
        let (Some(macos), Some(contents)) = (exe.parent(), exe.parent().and_then(Path::parent)) else {
            continue;
        };
        std::fs::create_dir_all(macos).map_err(|e| io("create a helper bundle", e))?;
        let name = format!("{HELPER_NAME}{suffix}");
        write_plist(
            contents.join("Info.plist"),
            info_plist(&name, &name, &helper_bundle_id(suffix), true),
        )?;
        if !is_fresh(&exe, &dev.helper_src) {
            let _ = std::fs::remove_file(&exe);
            std::fs::copy(&dev.helper_src, &exe).map_err(|e| io("copy pwrde-helper", e))?;
        }
    }
    Ok(())
}

/// `CefAppProtocol` for gpui's `NSApplication` class. Chromium asks the
/// application whether an event is being dispatched (to tell a nested run
/// loop from a top-level one) and sets the flag itself around its own
/// dispatches; a stock CEF app subclasses `NSApplication` for this, but gpui
/// owns the class, so the methods are added to it at runtime.
mod ns_app {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use objc::runtime::{
        BOOL, Class, Imp, NO, Object, Sel, YES, class_addMethod, class_addProtocol,
        class_getInstanceMethod, method_getImplementation,
        method_setImplementation, object_getClass, objc_getProtocol,
    };
    use objc::{class, msg_send, sel, sel_impl};

    static HANDLING_SEND_EVENT: AtomicBool = AtomicBool::new(false);
    /// The `sendEvent:` implementation ours wraps (gpui's own override, or
    /// `NSApplication`'s when gpui has none).
    static ORIGINAL_SEND_EVENT: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn is_handling_send_event(_: &Object, _: Sel) -> BOOL {
        if HANDLING_SEND_EVENT.load(Ordering::Relaxed) { YES } else { NO }
    }

    extern "C" fn set_handling_send_event(_: &Object, _: Sel, handling: BOOL) {
        HANDLING_SEND_EVENT.store(handling != NO, Ordering::Relaxed);
    }

    extern "C" fn send_event(this: &Object, sel: Sel, event: *mut Object) {
        let original = ORIGINAL_SEND_EVENT.load(Ordering::Relaxed);
        if original == 0 {
            return;
        }
        // Scoped, like `CefScopedSendingEvent`: a nested dispatch restores
        // the outer value rather than clearing the flag.
        let outer = HANDLING_SEND_EVENT.swap(true, Ordering::Relaxed);
        let original: extern "C" fn(&Object, Sel, *mut Object) =
            unsafe { std::mem::transmute(original) };
        original(this, sel, event);
        HANDLING_SEND_EVENT.store(outer, Ordering::Relaxed);
    }

    /// The shared application's class, or an error when gpui has not created
    /// its application yet.
    fn app_class() -> Result<*mut Class, String> {
        let app: *mut Object = unsafe { msg_send![class!(NSApplication), sharedApplication] };
        if app.is_null() {
            return Err("no NSApplication to host Chromium".into());
        }
        Ok(unsafe { object_getClass(app) } as *mut Class)
    }

    /// Add the protocol methods and wrap `sendEvent:`. Idempotent.
    pub fn install() -> Result<(), String> {
        let class = app_class()?;
        if ORIGINAL_SEND_EVENT.load(Ordering::Relaxed) != 0 {
            return Ok(());
        }
        // `BOOL` is a real bool on arm64 and a signed char on x86_64.
        let (getter_types, setter_types) = if cfg!(target_arch = "aarch64") {
            (c"B@:", c"v@:B")
        } else {
            (c"c@:", c"v@:c")
        };
        unsafe {
            let getter: extern "C" fn(&Object, Sel) -> BOOL = is_handling_send_event;
            let setter: extern "C" fn(&Object, Sel, BOOL) = set_handling_send_event;
            let sender: extern "C" fn(&Object, Sel, *mut Object) = send_event;
            class_addMethod(
                class,
                sel!(isHandlingSendEvent),
                std::mem::transmute::<_, Imp>(getter),
                getter_types.as_ptr(),
            );
            class_addMethod(
                class,
                sel!(setHandlingSendEvent:),
                std::mem::transmute::<_, Imp>(setter),
                setter_types.as_ptr(),
            );
            let method = class_getInstanceMethod(class, sel!(sendEvent:));
            if method.is_null() {
                return Err("NSApplication has no sendEvent:".into());
            }
            let original = method_getImplementation(method);
            ORIGINAL_SEND_EVENT.store(original as usize, Ordering::Relaxed);
            let sender = std::mem::transmute::<_, Imp>(sender);
            // Inherited from NSApplication: add an override that calls up.
            // Defined by gpui's class itself: replace it in place.
            if class_addMethod(class, sel!(sendEvent:), sender, c"v@:@".as_ptr()) == NO {
                method_setImplementation(method as *mut _, sender);
            }
        }
        Ok(())
    }

    /// Declare conformance once the framework — which defines the protocols —
    /// is loaded, for Chromium's `conformsToProtocol:` checks. A protocol the
    /// runtime does not know is skipped.
    pub fn adopt_protocols() {
        let Ok(class) = app_class() else { return };
        for name in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
            unsafe {
                let protocol = objc_getProtocol(name.as_ptr());
                if !protocol.is_null() {
                    class_addProtocol(class, protocol);
                }
            }
        }
    }
}

/// The `cef::App` handed to `cef::initialize`. The wrap macro resolves cef's
/// types unqualified, hence the glob import in a module of its own.
mod app {
    use cef::*;

    wrap_app! {
        pub struct PwrdeApp {
            mock_keychain: bool,
        }

        impl App {
            fn on_before_command_line_processing(
                &self,
                process_type: Option<&CefString>,
                command_line: Option<&mut CommandLine>,
            ) {
                // Browser process only (its type is empty).
                if process_type.is_some_and(|kind| !kind.to_string().is_empty()) {
                    return;
                }
                if let (true, Some(command_line)) = (self.mock_keychain, command_line) {
                    command_line.append_switch(Some(&CefString::from("use-mock-keychain")));
                }
            }
        }
    }
}

fn cef_path(path: &Path) -> cef::CefString {
    cef::CefString::from(path.to_string_lossy().as_ref())
}

/// Initialize CEF. Main thread only, after gpui's `NSApplication` exists and
/// before any browser is created. Never panics and never blocks launch: on
/// failure the error is kept for [`ready`] and webview tabs report it.
pub fn init() {
    let result = STATE.get_or_init(initialize);
    if let Err(error) = result {
        eprintln!("cef: {error}");
    }
}

fn initialize() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot resolve the executable: {e}"))?;
    let paths = locate(&exe, cef_dir().as_deref(), &|path| path.exists())?;
    if let Some(dev) = &paths.dev {
        prepare_dev_layout(dev)?;
    }
    ns_app::install()?;

    let library = paths.framework.join(FRAMEWORK_BIN);
    let library = std::ffi::CString::new(library.to_string_lossy().as_bytes())
        .map_err(|_| "the CEF framework path contains a NUL".to_string())?;
    if cef::load_library(Some(unsafe { &*library.as_ptr() })) != 1 {
        return Err(format!("cannot load {}", paths.framework.display()));
    }
    ns_app::adopt_protocols();
    // Pins the API version the bindings were generated for; must precede any
    // other CEF call.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    // Only argv[0]: pwrde's own arguments (a directory, a URL) are not
    // Chromium switches. Leaked — CEF may keep the pointers.
    let argv0 = std::ffi::CString::new(exe.to_string_lossy().as_bytes()).unwrap_or_default();
    let argv: &'static mut [*mut std::os::raw::c_char] =
        Box::leak(Box::new([argv0.into_raw(), std::ptr::null_mut()]));
    let args = cef::MainArgs { argc: 1, argv: argv.as_mut_ptr() };

    let cache = dirs::data_dir()
        .map(|data| cache_dir_in(&data, crate::git::worktree_scope().as_deref()))
        .ok_or("no data directory for the Chromium profile")?;
    std::fs::create_dir_all(&cache).map_err(|e| format!("create {}: {e}", cache.display()))?;
    let settings = cef::Settings {
        no_sandbox: 0,
        browser_subprocess_path: cef_path(&paths.helper),
        framework_dir_path: cef_path(&paths.framework),
        main_bundle_path: cef_path(&paths.main_bundle),
        // gpui runs the NSApplication loop; `pump` drives Chromium from it.
        external_message_pump: 1,
        root_cache_path: cef_path(&cache),
        log_file: cef_path(&cache.join("cef.log")),
        log_severity: cef::LogSeverity::WARNING,
        // Leave SIGINT/SIGTERM/SIGCHLD as pwrde (and its PTY children) had them.
        disable_signal_handlers: 1,
        ..Default::default()
    };
    let mut app = app::PwrdeApp::new(!crate::settings::get_bool("webview.keychain", false));
    if cef::initialize(Some(&args), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        return Err(format!(
            "Chromium failed to initialize (exit code {}; is another pwrde using {}?)",
            cef::get_exit_code(),
            cache.display()
        ));
    }
    Ok(())
}

/// Whether browsers can be created: `Err` carries why not.
pub fn ready() -> Result<(), String> {
    if SHUT_DOWN.load(Ordering::Relaxed) {
        return Err("Chromium has shut down".into());
    }
    STATE.get().cloned().unwrap_or_else(|| Err("Chromium is not initialized".into()))
}

/// Run Chromium's pending main-thread work once. Returns `false` without
/// doing anything when CEF is not up or the caller is already inside a pump
/// (CEF's loop must not nest).
pub fn pump() -> bool {
    if ready().is_err() || PUMPING.get() {
        return false;
    }
    PUMPING.set(true);
    cef::do_message_loop_work();
    PUMPING.set(false);
    true
}

/// A browser was created; pairs with [`browser_closed`].
pub fn browser_opened() {
    BROWSERS.fetch_add(1, Ordering::Relaxed);
}

/// `on_before_close` ran for a browser: Chromium is done with it.
pub fn browser_closed() {
    let _ = BROWSERS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
}

/// Ask the 16ms pump to shut CEF down and exit the process (`Action::Quit`,
/// which runs inside a gpui borrow where [`shutdown`] must not).
pub fn request_quit() {
    QUIT_REQUESTED.store(true, Ordering::Relaxed);
}

pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::Relaxed)
}

/// Shut CEF down on quit, from the 16ms pump only (see [`pump`]). The caller
/// has already asked every browser to close (`webview::Manager::close_all`);
/// closing completes asynchronously, so pump until the last one is gone —
/// bounded, quit must not hang — and only then call `cef::shutdown`.
/// Idempotent; a no-op when CEF never came up.
pub fn shutdown() {
    if ready().is_err() {
        return;
    }
    for _ in 0..SHUTDOWN_STEPS {
        if BROWSERS.load(Ordering::Relaxed) == 0 || !pump() {
            break;
        }
        std::thread::sleep(SHUTDOWN_STEP);
    }
    // A few more turns for the work the last close posted.
    for _ in 0..3 {
        pump();
    }
    shutdown_now();
}

/// `cef::shutdown` without waiting for browsers to finish closing: for a
/// caller inside a gpui borrow. Idempotent.
pub fn shutdown_now() {
    if ready().is_err() {
        return;
    }
    SHUT_DOWN.store(true, Ordering::Relaxed);
    cef::shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists_in<'a>(present: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |path| present.iter().any(|p| Path::new(p) == path)
    }

    #[test]
    fn locate_prefers_the_bundle_layout() {
        let exists = exists_in(&[
            "/Applications/Pwrde.app/Contents/Frameworks/Chromium Embedded Framework.framework",
        ]);
        let paths = locate(
            Path::new("/Applications/Pwrde.app/Contents/MacOS/pwrde"),
            Some(Path::new("/cef")),
            &exists,
        )
        .unwrap();
        assert_eq!(
            paths.framework,
            Path::new("/Applications/Pwrde.app/Contents/Frameworks/Chromium Embedded Framework.framework")
        );
        assert_eq!(
            paths.helper,
            Path::new(
                "/Applications/Pwrde.app/Contents/Frameworks/Pwrde Helper.app/Contents/MacOS/Pwrde Helper"
            )
        );
        assert_eq!(paths.main_bundle, Path::new("/Applications/Pwrde.app"));
        assert_eq!(paths.dev, None);
    }

    #[test]
    fn locate_falls_back_to_the_dev_layout_beside_the_binary() {
        let exists = exists_in(&[
            "/cef/Chromium Embedded Framework.framework",
            "/repo/target/debug/pwrde-helper",
        ]);
        let paths =
            locate(Path::new("/repo/target/debug/pwrde"), Some(Path::new("/cef")), &exists).unwrap();
        let frameworks = "/repo/target/debug/pwrde-cef/Pwrde.app/Contents/Frameworks";
        assert_eq!(
            paths.framework,
            Path::new(frameworks).join("Chromium Embedded Framework.framework")
        );
        assert_eq!(
            paths.helper,
            Path::new(frameworks).join("Pwrde Helper.app/Contents/MacOS/Pwrde Helper")
        );
        // The stand-in bundle is what CEF is told the main bundle is.
        assert_eq!(paths.main_bundle, Path::new("/repo/target/debug/pwrde-cef/Pwrde.app"));
        assert_eq!(
            paths.dev,
            Some(DevLayout {
                bundle: "/repo/target/debug/pwrde-cef/Pwrde.app".into(),
                framework_src: "/cef/Chromium Embedded Framework.framework".into(),
                helper_src: "/repo/target/debug/pwrde-helper".into(),
            })
        );
        // The helper resolves the framework three levels above its MacOS
        // directory (`cef::library_loader`): `Contents/Frameworks`.
        let up = paths.helper.parent().and_then(|p| p.parent()).and_then(|p| p.parent());
        assert_eq!(up.and_then(Path::parent), Some(Path::new(frameworks)));
        // A bundle-shaped path without the framework is not a bundle.
        let bare = locate(
            Path::new("/x/Pwrde.app/Contents/MacOS/pwrde"),
            Some(Path::new("/cef")),
            &exists_in(&[
                "/cef/Chromium Embedded Framework.framework",
                "/x/Pwrde.app/Contents/MacOS/pwrde-helper",
            ]),
        )
        .unwrap();
        assert!(bare.dev.is_some());
    }

    #[test]
    fn locate_reports_what_is_missing() {
        let exe = Path::new("/repo/target/debug/pwrde");
        let none = locate(exe, None, &exists_in(&[])).unwrap_err();
        assert!(none.contains("CEF_PATH"), "{none}");
        let no_framework = locate(exe, Some(Path::new("/cef")), &exists_in(&[])).unwrap_err();
        assert!(no_framework.contains("/cef"), "{no_framework}");
        let no_helper = locate(
            exe,
            Some(Path::new("/cef")),
            &exists_in(&["/cef/Chromium Embedded Framework.framework"]),
        )
        .unwrap_err();
        assert!(no_helper.contains("pwrde-helper") && no_helper.contains("cargo build"), "{no_helper}");
    }

    #[test]
    fn helper_exes_follow_chromium_naming() {
        let dir = Path::new("/f");
        assert_eq!(helper_exe(dir, ""), Path::new("/f/Pwrde Helper.app/Contents/MacOS/Pwrde Helper"));
        assert_eq!(
            helper_exe(dir, " (GPU)"),
            Path::new("/f/Pwrde Helper (GPU).app/Contents/MacOS/Pwrde Helper (GPU)")
        );
        assert_eq!(HELPER_SUFFIXES.len(), 5);
    }

    #[test]
    fn info_plists_name_their_executable_and_id() {
        assert_eq!(helper_bundle_id(""), "com.pwrde.terminal.helper");
        assert_eq!(helper_bundle_id(" (GPU)"), "com.pwrde.terminal.helper.gpu");
        assert_eq!(helper_bundle_id(" (Renderer)"), "com.pwrde.terminal.helper.renderer");
        let gpu = info_plist("Pwrde Helper (GPU)", "Pwrde Helper (GPU)", &helper_bundle_id(" (GPU)"), true);
        assert!(gpu.contains("<key>CFBundleExecutable</key>\n\t<string>Pwrde Helper (GPU)</string>"));
        assert!(gpu.contains("<string>com.pwrde.terminal.helper.gpu</string>"));
        assert!(gpu.contains("<key>LSUIElement</key>"));
        // The stand-in main bundle carries the app's own identifier — the
        // one browser and helpers derive their rendezvous name from.
        let main = info_plist("Pwrde", "pwrde", BUNDLE_ID, false);
        assert!(main.contains("<string>com.pwrde.terminal</string>"));
        assert!(!main.contains("LSUIElement"));
    }

    #[test]
    fn cef_dir_prefers_the_versioned_download_layout() {
        let root = Path::new("/cef");
        let versioned = exists_in(&[
            "/cef/154.0.32/cef_macos_aarch64/Chromium Embedded Framework.framework",
            "/cef/Chromium Embedded Framework.framework",
        ]);
        assert_eq!(
            cef_dir_in(root, "154.0.32+g682c378+chromium-154.0.8037.58", "aarch64", &versioned),
            Some("/cef/154.0.32/cef_macos_aarch64".into())
        );
        let flat = exists_in(&["/cef/Chromium Embedded Framework.framework"]);
        assert_eq!(cef_dir_in(root, "154.0.32", "aarch64", &flat), Some("/cef".into()));
        // Another version's download is not this build's framework.
        let other = exists_in(&["/cef/153.0.1/cef_macos_aarch64/Chromium Embedded Framework.framework"]);
        assert_eq!(cef_dir_in(root, "154.0.32", "aarch64", &other), None);
    }

    #[test]
    fn cache_dir_sits_beside_the_state_db() {
        let data = Path::new("/data");
        assert_eq!(cache_dir_in(data, None), Path::new("/data/pwrde/cef"));
        assert_eq!(
            cache_dir_in(data, Some("slug")),
            Path::new("/data/pwrde/worktrees/slug/cef")
        );
    }

    #[test]
    fn dev_layout_is_assembled_and_refreshed() {
        let root = std::env::temp_dir().join(format!("pwrde-cef-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dist = root.join("dist").join(FRAMEWORK_DIR);
        std::fs::create_dir_all(dist.join("Resources")).unwrap();
        std::fs::write(dist.join(FRAMEWORK_BIN), b"framework").unwrap();
        std::fs::write(dist.join("Resources").join("icudtl.dat"), b"icu").unwrap();
        std::fs::write(root.join(HELPER_BIN), b"helper-v1").unwrap();
        let dev = DevLayout {
            bundle: root.join(DEV_DIR).join(DEV_BUNDLE),
            framework_src: root.join("dist").join(FRAMEWORK_DIR),
            helper_src: root.join(HELPER_BIN),
        };
        prepare_dev_layout(&dev).unwrap();
        let frameworks = dev.bundle.join("Contents").join("Frameworks");
        assert!(dev.bundle.join("Contents").join("Info.plist").is_file());
        let framework = frameworks.join(FRAMEWORK_DIR);
        assert!(!framework.is_symlink());
        assert_eq!(std::fs::read(framework.join(FRAMEWORK_BIN)).unwrap(), b"framework");
        assert_eq!(std::fs::read(framework.join("Resources").join("icudtl.dat")).unwrap(), b"icu");
        for suffix in HELPER_SUFFIXES {
            let exe = helper_exe(&frameworks, suffix);
            assert_eq!(std::fs::read(&exe).unwrap(), b"helper-v1");
            assert!(exe.parent().unwrap().parent().unwrap().join("Info.plist").is_file());
        }
        // A rebuilt helper (different size) replaces every copy.
        std::fs::write(root.join(HELPER_BIN), b"helper-v2-longer").unwrap();
        prepare_dev_layout(&dev).unwrap();
        assert_eq!(std::fs::read(helper_exe(&frameworks, " (Renderer)")).unwrap(), b"helper-v2-longer");
        let _ = std::fs::remove_dir_all(&root);
    }
}
