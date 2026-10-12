//! Steinberg's VST3 validator (the SDK's `validator` sample), compiled by
//! the build script; this only passes it the command line (through
//! `src/shim.cpp`). Run it with a
//! `.vst3` bundle's path, or `-help` for its options.

use std::process::ExitCode;

#[cfg(not(windows))]
unsafe extern "C" {
    fn lspk_vst3_validator_run(
        argc: core::ffi::c_int,
        argv: *mut *mut core::ffi::c_char,
    ) -> core::ffi::c_int;
}

#[cfg(windows)]
unsafe extern "C" {
    fn lspk_vst3_validator_run_wide(
        argc: core::ffi::c_int,
        argv: *mut *mut u16,
    ) -> core::ffi::c_int;
}

fn main() -> ExitCode {
    #[cfg(not(windows))]
    let code = {
        use std::os::unix::ffi::OsStrExt;
        let args: Vec<std::ffi::CString> = std::env::args_os()
            .map(|a| std::ffi::CString::new(a.as_bytes()).expect("an argument without NUL"))
            .collect();
        let mut argv: Vec<*mut core::ffi::c_char> =
            args.iter().map(|a| a.as_ptr().cast_mut()).collect();
        argv.push(std::ptr::null_mut());
        let argc = core::ffi::c_int::try_from(args.len()).expect("few enough arguments");
        // SAFETY: argv holds argc NUL-terminated strings, then a null, as
        // C's main takes them, and they outlive the call.
        unsafe { lspk_vst3_validator_run(argc, argv.as_mut_ptr()) }
    };
    #[cfg(windows)]
    let code = {
        use std::os::windows::ffi::OsStrExt;
        let mut args: Vec<Vec<u16>> = std::env::args_os()
            .map(|a| a.encode_wide().chain([0]).collect())
            .collect();
        let mut argv: Vec<*mut u16> = args.iter_mut().map(|a| a.as_mut_ptr()).collect();
        argv.push(std::ptr::null_mut());
        let argc = core::ffi::c_int::try_from(args.len()).expect("few enough arguments");
        // SAFETY: argv holds argc NUL-terminated wide strings, then a null,
        // as wmain takes them, and they outlive the call.
        unsafe { lspk_vst3_validator_run_wide(argc, argv.as_mut_ptr()) }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
