//! The allocation trap and `rt_test!` as a test crate uses them.

lspk_rt::install_trap!();

use std::hint::black_box;
use std::process::Command;

lspk_rt::rt_test! {
    #[should_panic(expected = "in a real-time section")]
    fn a_test_that_allocates_under_the_trap_fails() {
        let samples: Vec<f32> = Vec::with_capacity(64);
        black_box(samples);
    }
}

lspk_rt::rt_test! {
    fn a_test_that_stays_off_the_heap_passes() {
        let mut phase = 0.0f32;
        for _ in 0..512 {
            phase = (phase + black_box(440.0 / 48_000.0)).fract();
        }
        assert!(phase < 1.0);
    }
}

/// Fails on purpose, so the test below can show what the failure looks like.
#[test]
#[ignore = "fails on purpose; `a_failure_says_what_allocated_and_where` runs it"]
fn allocates_under_the_trap() {
    lspk_rt::rt_test!({
        black_box(Box::new([0u64; 8]));
    });
}

#[test]
fn a_failure_says_what_allocated_and_where() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "allocates_under_the_trap",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: FAILED. 0 passed; 1 failed"),
        "{stdout}"
    );
    assert!(
        stderr.contains(
            "2 heap operations in a real-time section; the first allocated 64 bytes, at:"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("trap::allocates_under_the_trap"),
        "the backtrace leads to the test: {stderr}"
    );
}
