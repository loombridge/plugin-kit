//! The plug-in kit's `cargo xtask`. Run `cargo xtask help` for the list.

fn main() -> std::process::ExitCode {
    lspk_xtask::main(&lspk_xtask::KIT)
}
