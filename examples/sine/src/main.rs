//! Example Sine as a standalone app, with the agent interface, as its
//! plug-in exports.

fn main() -> std::process::ExitCode {
    lspk_standalone::run::<lspk_plugin_agent::WithAgent<lspk_example_sine::Sine>>()
}
