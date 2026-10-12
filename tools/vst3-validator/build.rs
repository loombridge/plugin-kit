//! Compiles Steinberg's VST3 validator from the VST3 SDK sources vendored in
//! `crates/clap-wrapper/vendor/vst3sdk`, following the SDK's own CMake
//! targets (`validator`, `sdk_hosting` and `sdk_common`), with its `main`
//! renamed so `src/main.rs` can call it.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("the manifest dir"));
    let sdk = manifest.join("../../crates/clap-wrapper/vendor/vst3sdk");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/shim.cpp");
    println!("cargo:rerun-if-changed={}", sdk.display());
    let os = std::env::var("CARGO_CFG_TARGET_OS").expect("a target OS");
    let debug = std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    if os == "macos" && std::env::var_os("MACOSX_DEPLOYMENT_TARGET").is_none() {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let version = if arch == "aarch64" { "11.0" } else { "10.13" };
        // SAFETY: the build script is single-threaded until `cc` compiles.
        unsafe { std::env::set_var("MACOSX_DEPLOYMENT_TARGET", version) };
    }

    let mut cc = cc::Build::new();
    cc.cpp(true)
        .std("c++17")
        .warnings(false)
        .extra_warnings(false);
    cc.flag_if_supported("-w");
    cc.flag_if_supported("/w");
    cc.flag_if_supported("/utf-8");
    cc.flag_if_supported("/EHsc");
    cc.include(&sdk);
    cc.define(if debug { "DEVELOPMENT" } else { "RELEASE" }, Some("1"));
    cc.define("main", Some("lspk_vst3_validator_main"));
    cc.define("wmain", Some("lspk_vst3_validator_wmain"));

    // sdk_common
    cc.files(walk(&sdk.join("base/source"), "cpp"));
    cc.files(walk(&sdk.join("base/thread/source"), "cpp"));
    cc.files(walk(&sdk.join("pluginterfaces/base"), "cpp"));
    let public = sdk.join("public.sdk/source");
    cc.files(walk(&public.join("common"), "cpp"));
    // sdk_hosting
    for file in [
        "vst/hosting/connectionproxy.cpp",
        "vst/hosting/eventlist.cpp",
        "vst/hosting/hostclasses.cpp",
        "vst/hosting/module.cpp",
        "vst/hosting/parameterchanges.cpp",
        "vst/hosting/pluginterfacesupport.cpp",
        "vst/hosting/processdata.cpp",
        "vst/utility/stringconvert.cpp",
        "vst/vstinitiids.cpp",
        "vst/vstpresetfile.cpp",
    ] {
        cc.file(public.join(file));
    }
    // validator
    for file in [
        "main/moduleinit.cpp",
        "vst/moduleinfo/moduleinfoparser.cpp",
        "vst/hosting/plugprovider.cpp",
        "vst/utility/testing.cpp",
    ] {
        cc.file(public.join(file));
    }
    cc.files(walk(&public.join("vst/hosting/test"), "cpp"));
    cc.files(walk(&public.join("vst/testsuite"), "cpp"));
    cc.files(walk(
        &sdk.join("public.sdk/samples/vst-hosting/validator/source"),
        "cpp",
    ));
    cc.file(manifest.join("src/shim.cpp"));

    match os.as_str() {
        "macos" => {
            cc.file(public.join("vst/hosting/module_mac.mm"))
                .file(public.join("common/threadchecker_mac.mm"));
            cc.flag("-fobjc-arc");
            for framework in ["Cocoa", "CoreFoundation", "Foundation"] {
                println!("cargo:rustc-link-lib=framework={framework}");
            }
        }
        "windows" => {
            cc.file(public.join("vst/hosting/module_win32.cpp"));
            for lib in ["ole32", "shell32", "user32"] {
                println!("cargo:rustc-link-lib={lib}");
            }
        }
        _ => {
            cc.file(public.join("vst/hosting/module_linux.cpp"));
            println!("cargo:rustc-link-lib=dl");
        }
    }
    cc.compile("lspk_vst3_validator");
}

/// Every file under `dir` with extension `ext`, in a stable order.
fn walk(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("couldn't list {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == ext) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
