//! Embeds `scripts/privacy-usage.plist` in the bare `pwrde` executable as its
//! `__TEXT,__info_plist` section. A binary run outside an app bundle
//! (`cargo run`) has no `Info.plist`, and macOS aborts a process that touches
//! Bluetooth, the camera or the microphone without a usage string — which a
//! Chromium web tab does on any passkey sign-in page. In `Pwrde.app` the
//! bundle's own `Info.plist` (which `scripts/make-app.sh` merges the same
//! file into) takes precedence.

fn main() {
    let plist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("scripts/privacy-usage.plist");
    println!("cargo:rerun-if-changed={}", plist.display());
    println!(
        "cargo:rustc-link-arg-bin=pwrde=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}
