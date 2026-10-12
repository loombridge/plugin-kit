//! Example Gain as a standalone app.

fn main() -> std::process::ExitCode {
    lspk_standalone::run::<lspk_example_gain::Gain>()
}
