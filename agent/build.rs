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
//!
//! Also carries MMSSTV's PTT plugin (the `mmsstv-ptt` package, built for
//! 32-bit Windows) inside the Agent, when `LABLOG_MMSSTV_PTT_DLL` names the
//! built DLL: CI's Windows job builds it first. The file is checked before
//! it is carried: a 32-bit DLL exporting the five names MMSSTV looks up
//! (see `mmsstv`). Without the variable the Agent is built without it and
//! says so.

#[path = "src/pe_exports.rs"]
#[allow(dead_code)]
mod pe_exports;

/// The names MMSSTV looks up in a PTT plugin (its `Comm.cpp`, `CEXTFSK`).
const MMSSTV_NAMES: [&str; 5] = [
    "extfskOpen",
    "extfskClose",
    "extfskIsTxBusy",
    "extfskPutChar",
    "extfskSetPTT",
];

/// Carries MMSSTV's PTT plugin, if one was built; a wrong file stops the
/// build.
fn mmsstv_plugin() {
    println!("cargo::rustc-check-cfg=cfg(mmsstv_plugin)");
    println!("cargo:rerun-if-env-changed=LABLOG_MMSSTV_PTT_DLL");
    let Ok(path) = std::env::var("LABLOG_MMSSTV_PTT_DLL") else {
        return;
    };
    let path = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        Err(why) => {
            println!("cargo::error=LABLOG_MMSSTV_PTT_DLL ({path}): {why}");
            return;
        }
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let bytes = std::fs::read(&path).unwrap_or_default();
    let Some(exports) = pe_exports::read(&bytes) else {
        println!("cargo::error={} is not a DLL", path.display());
        return;
    };
    if exports.machine != pe_exports::I386 {
        println!(
            "cargo::error={} is not 32-bit (machine {:#x}); MMSSTV is 32-bit",
            path.display(),
            exports.machine
        );
        return;
    }
    let missing: Vec<&str> = MMSSTV_NAMES
        .into_iter()
        .filter(|name| !exports.names.iter().any(|found| found == name))
        .collect();
    if !missing.is_empty() {
        println!(
            "cargo::error={} doesn't export {missing:?} (it exports {:?})",
            path.display(),
            exports.names
        );
        return;
    }
    println!("cargo:rustc-cfg=mmsstv_plugin");
    println!("cargo:rustc-env=LABLOG_MMSSTV_PTT_FILE={}", path.display());
}

fn main() {
    mmsstv_plugin();
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
