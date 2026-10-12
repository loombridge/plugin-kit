//! Builds free-audio's clap-wrapper and the SDK sources it needs, all
//! vendored in `vendor/`, into static libraries with `cc`: the VST3 wrapper
//! (feature `vst3`) on every platform, and the Audio Unit v2 wrapper
//! (feature `auv2`) on macOS. Nothing is downloaded.
//!
//! The wrappers find the plug-in through its `clap_entry` symbol, linked into
//! the same library (`STATICALLY_LINKED_CLAP_ENTRY`), and their entry points
//! are renamed (`clap_wrapper_GetPluginFactory` and so on) so `src/lib.rs`
//! can export them from Rust.
//!
//! Adapted from clap-wrapper-rs (<https://github.com/blepfx/clap-wrapper-rs>),
//! MIT OR Apache-2.0.

use std::path::{Path, PathBuf};

/// clap-wrapper's version, as its own build passes it.
const CLAP_WRAPPER_VERSION: &str = "0.14.0";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=cpp");
    println!("cargo:rerun-if-env-changed=LSPK_CLAP_WRAPPER_LOGLEVEL");

    let os = std::env::var("CARGO_CFG_TARGET_OS").expect("a target OS");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let host = std::env::var("HOST").unwrap_or_default();
    let debug = std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    let vst3 = std::env::var_os("CARGO_FEATURE_VST3").is_some();
    let auv2 = std::env::var_os("CARGO_FEATURE_AUV2").is_some() && os == "macos";

    // Checking a Windows build from macOS or Linux (as the kit's Windows
    // clippy line does) can't compile C++ against the Windows SDK, and
    // doesn't link, so the wrappers are left out of it.
    if target_env == "msvc" && !host.contains("windows") {
        if vst3 {
            println!(
                "cargo:warning=the VST3 wrapper isn't compiled when cross-checking for MSVC; \
                 a build for Windows has to run on Windows"
            );
        }
        return;
    }

    // Compile for the macOS rustc links for (its default, unless the build
    // sets one), not the one the build machine runs.
    if os == "macos" && std::env::var_os("MACOSX_DEPLOYMENT_TARGET").is_none() {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let version = if arch == "aarch64" { "11.0" } else { "10.12" };
        // SAFETY: the build script is single-threaded until `cc` compiles,
        // after this.
        unsafe { std::env::set_var("MACOSX_DEPLOYMENT_TARGET", version) };
    }

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("the manifest dir"));
    let vendor = root.join("vendor");
    if vst3 {
        build_vst3(&vendor, &os, debug);
    }
    if auv2 {
        build_auv2(&root, &vendor, debug);
    }
}

/// A C++17 build of vendored code: quiet, since its warnings aren't ours to
/// fix, and in the SDKs' development or release mode.
fn cpp(debug: bool) -> cc::Build {
    let mut cc = cc::Build::new();
    cc.cpp(true)
        .std("c++17")
        .warnings(false)
        .extra_warnings(false);
    cc.flag_if_supported("-w");
    cc.flag_if_supported("/w");
    cc.flag_if_supported("/utf-8");
    cc.flag_if_supported("/EHsc");
    cc.define(
        "CLAP_WRAPPER_VERSION",
        Some(format!("\"{CLAP_WRAPPER_VERSION}\"").as_str()),
    );
    cc.define("STATICALLY_LINKED_CLAP_ENTRY", Some("1"));
    // clap-wrapper logs to standard output, unless built with level 0; set
    // LSPK_CLAP_WRAPPER_LOGLEVEL (1 or 2) to see its log.
    let level = std::env::var("LSPK_CLAP_WRAPPER_LOGLEVEL").unwrap_or_else(|_| "0".into());
    cc.define("CLAP_WRAPPER_LOGLEVEL", Some(level.as_str()));
    if debug {
        cc.define("DEVELOPMENT", Some("1"));
    } else {
        cc.define("RELEASE", Some("1"));
    }
    cc
}

fn build_vst3(vendor: &Path, os: &str, debug: bool) {
    let sdk = vendor.join("vst3sdk");
    let wrapper = vendor.join("clap-wrapper");
    let mut cc = cpp(debug);
    cc.include(vendor.join("clap/include"))
        .include(wrapper.join("include"))
        .include(wrapper.join("libs/fmt"))
        .include(wrapper.join("libs/psl"))
        .include(wrapper.join("src"))
        .include(&sdk)
        .include(sdk.join("public.sdk"))
        .include(sdk.join("pluginterfaces"));
    cc.define("CLAP_WRAPPER_BUILD_FOR_VST3", Some("1"));
    // Renamed, so the Rust side exports them (see src/lib.rs).
    for symbol in [
        "GetPluginFactory",
        "ModuleEntry",
        "ModuleExit",
        "InitDll",
        "ExitDll",
        "bundleEntry",
        "bundleExit",
    ] {
        cc.define(symbol, Some(format!("clap_wrapper_{symbol}").as_str()));
    }

    cc.files(walk(&sdk.join("base/source"), "cpp"));
    cc.files(walk(&sdk.join("base/thread/source"), "cpp"));
    cc.files(walk(&sdk.join("pluginterfaces/base"), "cpp"));
    let public = sdk.join("public.sdk/source");
    cc.files(walk(&public.join("common"), "cpp"));
    for file in [
        "main/pluginfactory.cpp",
        "main/moduleinit.cpp",
        "vst/vstinitiids.cpp",
        "vst/vstnoteexpressiontypes.cpp",
        "vst/vstaudioeffect.cpp",
        "vst/vstcomponent.cpp",
        "vst/vstsinglecomponenteffect.cpp",
        "vst/vstcomponentbase.cpp",
        "vst/vstbus.cpp",
        "vst/vstparameters.cpp",
        "vst/utility/stringconvert.cpp",
    ] {
        cc.file(public.join(file));
    }
    let src = wrapper.join("src");
    for file in [
        "clap_proxy.cpp",
        "detail/clap/fsutil.cpp",
        "detail/shared/sha1.cpp",
        "wrapasvst3_export_entry.cpp",
        "wrapasvst3.cpp",
        "wrapasvst3_entry.cpp",
        "detail/vst3/parameter.cpp",
        "detail/vst3/plugview.cpp",
        "detail/vst3/process.cpp",
        "detail/vst3/categories.cpp",
    ] {
        cc.file(src.join(file));
    }

    match os {
        "macos" => {
            cc.file(public.join("main/macmain.cpp"))
                .file(src.join("detail/os/macos.mm"))
                .file(src.join("detail/clap/mac_helpers.mm"))
                .define("MAC", None)
                .define("MACOS_USE_GHC_FILESYSTEM", None)
                .include(vendor.join("filesystem/include"));
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        "windows" => {
            cc.file(public.join("main/dllmain.cpp"))
                .file(src.join("detail/os/windows.cpp"))
                .define("WIN", None);
            for lib in ["shell32", "user32", "ole32"] {
                println!("cargo:rustc-link-lib={lib}");
            }
        }
        "linux" => {
            cc.file(public.join("main/linuxmain.cpp"))
                .file(src.join("detail/os/linux.cpp"))
                .define("LIN", None);
        }
        other => panic!("the VST3 wrapper doesn't support {other}"),
    }
    cc.compile("lspk_clap_wrapper_vst3");
}

fn build_auv2(root: &Path, vendor: &Path, debug: bool) {
    let wrapper = vendor.join("clap-wrapper");
    let mut cc = cpp(debug);
    cc.flag_if_supported("-fno-char8_t");
    cc.include(root.join("cpp"))
        .include(vendor.join("clap/include"))
        .include(wrapper.join("include"))
        .include(wrapper.join("libs/fmt"))
        .include(wrapper.join("src"))
        .include(vendor.join("AudioUnitSDK/include"))
        .include(vendor.join("filesystem/include"));
    cc.define("CLAP_WRAPPER_BUILD_AUV2", Some("1"))
        .define("DICTIONARY_STREAM_FORMAT_WRAPPER", Some("1"))
        .define("MAC", None)
        .define("MACOS_USE_GHC_FILESYSTEM", None);
    // Objective-C classes share one namespace per process, so each build
    // names its own: two plug-ins built in different places never clash.
    let suffix = fnv1a(std::env::var("OUT_DIR").unwrap_or_default().as_bytes());
    cc.define(
        "CLAP_WRAPPER_COCOA_CLASS_NSVIEW",
        Some(format!("lspk_cocoaUI_nsview_{suffix:08x}").as_str()),
    );
    cc.define(
        "CLAP_WRAPPER_COCOA_CLASS",
        Some(format!("lspk_cocoaUI_{suffix:08x}").as_str()),
    );

    let sdk = vendor.join("AudioUnitSDK/src/AudioUnitSDK");
    for file in [
        "AUBase.cpp",
        "AUBuffer.cpp",
        "AUBufferAllocator.cpp",
        "AUEffectBase.cpp",
        "AUMIDIBase.cpp",
        "AUMIDIEffectBase.cpp",
        "AUInputElement.cpp",
        "AUOutputElement.cpp",
        "AUScopeElement.cpp",
        "AUPlugInDispatch.cpp",
        "ComponentBase.cpp",
        "MusicDeviceBase.cpp",
    ] {
        cc.file(sdk.join(file));
    }
    let src = wrapper.join("src");
    for file in [
        "clap_proxy.cpp",
        "detail/clap/fsutil.cpp",
        "detail/shared/sha1.cpp",
        "wrapasauv2.cpp",
        "detail/auv2/auv2_shared.mm",
        "detail/auv2/process.cpp",
        "detail/auv2/wrappedview.mm",
        "detail/auv2/parameter.cpp",
        "detail/os/macos.mm",
        "detail/clap/mac_helpers.mm",
    ] {
        cc.file(src.join(file));
    }
    for framework in [
        "Foundation",
        "CoreFoundation",
        "AudioToolbox",
        "CoreMIDI",
        "AppKit",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    cc.compile("lspk_clap_wrapper_auv2");
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

/// The 32-bit FNV-1a hash of `bytes`.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, &b| {
        (hash ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}
