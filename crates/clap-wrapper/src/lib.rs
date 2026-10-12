//! VST3 and Audio Unit v2 entry points for a CLAP plug-in, made by
//! free-audio's [clap-wrapper](https://github.com/free-audio/clap-wrapper)
//! from the `clap_entry` symbol in the same library.
//!
//! A plug-in doesn't use this crate directly: `lspk-plugin`'s `vst3` and
//! `auv2` features turn it on, and `export_clap!` exports what it holds, so
//! one library is a CLAP, VST3 and (on macOS) Audio Unit plug-in at once.
//! `cargo xtask bundle` lays that library out as each format's bundle.
//!
//! # What's vendored
//!
//! The build script compiles C++ from `vendor/`, copied from these upstream
//! revisions, each with its licence file. Nothing is downloaded at build
//! time. `cargo xtask vendored` checks every component is listed here with a
//! licence the kit allows, and that no vendored file claims stricter terms.
//!
//! | Component | Revision | Licence |
//! |---|---|---|
//! | `clap-wrapper` (free-audio) | 0.14.0, `18a95457242849eb71d2039f8af532f8aed437bb`, without the standalone wrapper | MIT |
//! | `clap-wrapper/libs/fmt` ({fmt}, in clap-wrapper) | as above | MIT |
//! | `clap-wrapper/libs/psl` (PreSonus plug-in extensions, in clap-wrapper) | as above | Public domain |
//! | `clap` headers (free-audio) | 1.2.7, `29ffcc273be7c7c651f6c9953b99e69700e2387a` | MIT |
//! | VST3 SDK (Steinberg): `base`, `pluginterfaces`, part of `public.sdk` | 3.8.0: base `3d2e82f8e6bff59c1d8b7a27491a29c2286b5206`, pluginterfaces `31d6eeba6daaa3e2a8bfbe3e7a90ca0b7fbfbc1c`, public.sdk `a3911a4615dabbfdfd9d181ee26b05c70c289a95` | MIT |
//! | `AudioUnitSDK` (Apple) | `53a9a2008aae7fb1b0a9f093dd523b9b12f6c0d9` | Apache-2.0 |
//! | `filesystem` (ghc::filesystem, for macOS before 10.15) | `0e72911ba0f37f3eb883b181b855a5f4eb046ca1` | MIT |
//!
//! These are the revisions clap-wrapper-rs 0.3.1 pins. Of `public.sdk`,
//! only `source/` (without the AAX, Audio Unit, AUv3, inter-app audio and
//! VSTGUI wrappers) and the validator's sources under
//! `samples/vst-hosting/validator` are kept; the validator is built by
//! `tools/vst3-validator`. `cpp/` holds the two files clap-wrapper's CMake
//! build would generate. The build script and this file are adapted from
//! clap-wrapper-rs (MIT OR Apache-2.0).
//!
//! Three changes from upstream:
//!
//! - `vst3sdk/base/source/hexbinary.h` is left out: unused, and its header
//!   still carries the SDK's licence from before 3.8, not MIT.
//! - In `clap-wrapper/src/wrapasvst3.cpp`, the parameter flush the wrapper
//!   runs from its idle timer while the plug-in is active but not
//!   processing passes the wrapper itself as the automation sink (`this`,
//!   where upstream passes `nullptr`), so the plug-in's gestures from that
//!   flush reach the host as edits instead of being dropped.
//! - In the same file, a parameter rescan with CLAP's info flag also
//!   refreshes each parameter's short title and hidden flag, not only its
//!   title, so a slot whose module changes shows a VST3 host the
//!   parameters it now uses (`restartComponent` with
//!   `kParamTitlesChanged`, which covers flags).
//!
//! # Formats
//!
//! - **VST3**: clap-wrapper derives the VST3 class ID from the CLAP ID, maps
//!   CLAP's features to VST3's categories, parameters one to one (values
//!   normalised over each parameter's CLAP range), the plug-in's gestures to
//!   `IComponentHandler`'s `beginEdit`, `performEdit` and `endEdit` (from its
//!   idle timer, on the main thread), `mark_dirty` to
//!   `IComponentHandler2::setDirty`, and a parameter rescan to
//!   `restartComponent`: the values flag to `kParamValuesChanged`, the info
//!   flag to `kParamValuesChanged | kParamTitlesChanged`, after refreshing
//!   its own copy of each value, title and hidden flag. A CLAP module path
//!   becomes a VST3 unit when the parameters are first set up.
//! - **Audio Unit v2**: a parameter rescan of any kind re-reads every
//!   parameter's info (name, and the readable and writable flags a hidden
//!   parameter loses) and notifies `kAudioUnitProperty_ParameterList`,
//!   `kAudioUnitProperty_ParameterInfo` and `kAudioUnitProperty_ClassInfo`.
//! - **Audio Unit v2**: the type, subtype and manufacturer come from the
//!   bundle's `Info.plist`, which `cargo xtask bundle` writes from the
//!   plug-in's info; gestures become `AUEventListenerNotify` calls.

/// The VST3 wrapper's entry points, renamed. `lspk-plugin`'s
/// `export_clap!` exports them under their own names (`GetPluginFactory`,
/// and the platform's load and unload calls) from the plug-in's library,
/// so only a library that also holds `clap_entry` links them.
#[cfg(feature = "vst3")]
#[doc(hidden)]
pub mod vst3 {
    use core::ffi::c_void;

    unsafe extern "system" {
        pub fn clap_wrapper_GetPluginFactory() -> *mut c_void;
    }

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        pub fn clap_wrapper_bundleEntry(bundle: *mut c_void) -> bool;
        pub fn clap_wrapper_bundleExit() -> bool;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    unsafe extern "C" {
        pub fn clap_wrapper_ModuleEntry(module: *mut c_void) -> bool;
        pub fn clap_wrapper_ModuleExit() -> bool;
    }

    #[cfg(windows)]
    unsafe extern "system" {
        pub fn clap_wrapper_InitDll() -> bool;
        pub fn clap_wrapper_ExitDll() -> bool;
    }
}

/// The Audio Unit wrapper's factory function, which `export_clap!` exports
/// as [`auv2::FACTORY`], the name the bundle's `Info.plist` gives.
#[cfg(all(feature = "auv2", target_os = "macos"))]
#[doc(hidden)]
pub mod auv2 {
    use core::ffi::c_void;

    /// The factory function's exported name, as `Info.plist` names it.
    pub const FACTORY: &str = "GetPluginFactoryAUV2";

    unsafe extern "C" {
        pub fn lspkWrapAsAUV2Factory(desc: *const c_void) -> *mut c_void;
    }
}
