// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Asks Windows for version 6 of its common controls library, which the
//! tray's window code needs.
//!
//! The tray (`native-windows-gui`) calls `SetWindowSubclass` and
//! `DefSubclassProc`, which "ComCtl32.dll version 6 contains" (Microsoft
//! Learn, "Subclassing Controls"); without asking, a program gets version 5
//! and Windows refuses to start it (`STATUS_ENTRYPOINT_NOT_FOUND`). Microsoft
//! Learn ("Enabling Visual Styles") gives this linker directive as the way
//! to ask for version 6 without a separate manifest file.

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    println!("cargo:rerun-if-changed=build.rs");
}
