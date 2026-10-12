# Contributing to {{Name}}

Thank you for helping build {{Name}}. This page explains how a change gets from your machine into `main`: what to read first, how work is organised, the checks every change passes, and the certificate you give with each commit.

## Read first

1. [README.md](README.md) and [AGENTS.md](AGENTS.md): what {{Name}} is, the lines no change crosses, and how work is done.
1. [The licence rule](#the-licence-rule), before you write or port anything.

## The licence rule

{{#permissive}}
{{Name}} is MIT OR Apache-2.0, as the plug-in kit is. That means:

- **No copyleft dependencies.** `cargo deny` rejects GPL, LGPL and AGPL crates, and the weak copyleft licences aren't on its allow-list either.
- **No code derived from copyleft projects.** Don't port, translate or closely paraphrase code from GPL projects, however useful. Papers, textbooks, open standards and permissively licensed code are fine, within their licences; name the source in a comment.
{{/permissive}}
{{#gpl}}
{{Name}} is GPL-3.0-or-later. That means:

- **Dependencies must combine with GPL-3.0-or-later.** `cargo deny` checks every crate's licence against `deny.toml`.
- **Code from GPL-3.0 projects is welcome, within their licences,** with a header comment in each derived file naming its source. That code stays in this repository and never goes into the plug-in kit, which is MIT OR Apache-2.0.
{{/gpl}}
- **No one else's content.** No samples, presets, wavetables, impulse responses, fonts or artwork you didn't make, and no plug-in or SDK binaries.

## Find something to work on

- **Bugs and features.** Open an issue first.
- **Small fixes**, such as a typo or a broken link, can go straight to a pull request.

## Make the change

1. Give every feature a headless test path. A feature that can only be tested by clicking isn't done.
1. Register every user-facing action as a command, with a schema, a model-facing description and a permission class, so agents can reach it.
1. Keep to the rules in [AGENTS.md](AGENTS.md): no allocation, lock, or blocking call on an audio thread; parameter IDs never change meaning; no dependency that `cargo deny` rejects.
1. If the change would serve a second plug-in too, it belongs in the plug-in kit: make it there first (see the next section).
1. Run the checks. They must pass before you open the pull request.

   ```sh
   cargo xtask check
   cargo xtask validate
   ```

   The first runs format, clippy with warnings denied, the tests under cargo-nextest (the golden renders, measurements and the parameter ID list among them) and then the doctests, the layer rules, `cargo deny`, and a self-test that proves `deny.toml` still rejects the licences it must. The second runs every plug-in format's validators.

## Working on the plug-in kit and {{Name}} at once

{{Name}} depends on the [plug-in kit](https://github.com/loombridge/plugin-kit) by git revision: every `lspk-` crate in the workspace `Cargo.toml` names the same `rev`. When a change needs the kit too, point Cargo at a local checkout of the kit with a `[patch]` section, so both build together as you work.

1. Clone the kit beside {{Name}}, so the folder holding both looks like this, and start a branch in each:

   ```text
   code/
     plugin-kit/
     {{crate}}/
   ```

1. Create `.cargo/config.toml` in the folder that holds both checkouts (`code/` above), not in either repository, with one line for each kit crate {{Name}} names. Paths are relative to that folder:

   ```toml
   [patch."https://github.com/loombridge/plugin-kit"]
{{kit_patch}}
   ```

   Cargo reads it from every checkout below that folder, and it patches only crates that come from the kit's repository. {{Name}} now builds against your kit checkout, and `cargo xtask check` passes in both repositories as you work. Cargo rewrites `Cargo.lock` to drop the kit's git source while the patch is on; don't commit it in that state.

1. When the kit's half is ready, push its branch and open its pull request.
1. Turn the patch off (rename or delete that `.cargo/config.toml`), set `rev` in {{Name}}'s `Cargo.toml` to the kit commit your change needs, on every `lspk-` line, and run `cargo xtask check`. Cargo updates `Cargo.lock` to the new revision. Commit `Cargo.toml` and `Cargo.lock` with the rest of the change, and say in {{Name}}'s pull request which kit pull request it needs.
1. The kit's pull request merges first, with a merge commit, so the revision stays reachable from the kit's `main`. {{Name}}'s then merges, pinned to that revision or to the kit's `main` after it.

To move to a newer kit without changing it, set every `rev` to a newer commit on the kit's `main` and run `cargo xtask check`; a kit change can move the golden renders or the benchmark counts, which the checks report.

## Commit

- Stage files by path. Never use `git add -A` or `git add .`, and check `git status` so build output, `.DS_Store`, and scratch files stay out.
- Sign off every commit, as the next section explains.

## Developer Certificate of Origin

{{Name}} takes contributions under the [Developer Certificate of Origin](https://developercertificate.org) (DCO), version 1.1, instead of a contributor licence agreement. You keep the copyright in your work. When you sign off a commit, you certify the statements below: in short, that you wrote the change, or otherwise have the right to submit it under the project's open-source licence, and that the record of your contribution is public.

```text
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

### How to sign off

A sign-off is a `Signed-off-by` line at the end of the commit message, with your name and email address. Git adds it from your `user.name` and `user.email` settings when you commit with `-s`:

```sh
git commit -s -m "Fix the filter's drive range"
```

The message then ends with a line like this one:

```text
Signed-off-by: Your Name <you@example.com>
```

Use the name you're known by in the project and an email address that reaches you. Anonymous sign-offs aren't accepted.

If you forget:

- For the last commit, run `git commit --amend --no-edit --signoff`.
- For every commit on your branch, run `git rebase --signoff origin/main`, then `git push --force-with-lease`.

To see which commits on your branch are signed off, run:

```sh
git log origin/main..HEAD --format='%h %s%n  %(trailers:key=Signed-off-by,valueonly,separator=%x2C )'
```

A commit with nothing on the indented line under it isn't signed off.

### Changes written with an AI agent

If an AI coding agent wrote some or all of your change, you're still the person certifying it. Review the change, then sign it off yourself. The agent records its part with a `Co-Authored-By` trailer, and never adds a `Signed-off-by` line, in your name or its own, because only a person can give the certificate.

Commits made by the project itself, authored as Loombridge, carry no sign-off.

### Checking sign-offs

For now, the reviewer checks sign-offs by hand.

## Open a pull request

- Open it against `main`.
- Say what the change adds, paste your `cargo xtask check` summary, and list anything that needs a maintainer.
- Update any document whose facts your change alters, in the same pull request.
- A maintainer reviews and merges. Don't merge your own pull request.

## Write in the house style

Write prose, including documents, code comments, commit messages, and pull requests, in plain British English, with sentence-case headings. Address the reader in the second person. `rustfmt` and `clippy` settle the code's formatting, and `cargo xtask check` runs both.

## Licence

{{#permissive}}
{{Name}} is licensed under either of Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT ([LICENSE-MIT](LICENSE-MIT)), at your option, and you contribute under the same terms, certifying each commit with the Developer Certificate of Origin.
{{/permissive}}
{{#gpl}}
{{Name}} is licensed under GPL-3.0-or-later ([LICENSE](LICENSE)), and you contribute under the same licence, certifying each commit with the Developer Certificate of Origin.
{{/gpl}}
