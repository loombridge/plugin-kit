//! {{Name}}'s `cargo xtask`: the plug-in kit's tasks, run in this repository.
//! Run `cargo xtask help` for the list.

use lspk_xtask::Repo;

/// This repository to the kit's tasks: its plug-in crate, which `cargo xtask
/// validate` checks when none is named, and the licences in the kit's
/// fixture that deny.toml must reject (`cargo xtask deny-selftest`).
{{#permissive}}
const REPO: Repo = Repo::plugin(&["{{crate}}"], lspk_xtask::REJECTS_COPYLEFT);
{{/permissive}}
{{#gpl}}
const REPO: Repo = Repo::plugin(&["{{crate}}"], lspk_xtask::REJECTS_AGPL);
{{/gpl}}

fn main() -> std::process::ExitCode {
    lspk_xtask::main(&REPO)
}
