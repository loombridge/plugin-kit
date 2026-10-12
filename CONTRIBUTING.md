# Contributing to the plug-in kit

Thank you for helping build the plug-in kit. This page explains how a change gets from your machine into `main`: what to read first, how work is organised, the checks every change passes, and the certificate you give with each commit.

## Read first

1. [README.md](README.md) and [AGENTS.md](AGENTS.md): what the kit holds, the lines no change crosses, and how work is done.
1. [The licence rule](#the-licence-rule), before you write or port anything.

## The licence rule

The kit is MIT OR Apache-2.0 so that any plug-in can build on it, whatever its own licence. That means:

- **No copyleft dependencies.** `cargo deny` rejects GPL, LGPL and AGPL crates, and the weak copyleft licences aren't on its allow-list either.
- **No code derived from copyleft projects.** Don't port, translate or closely paraphrase code from GPL projects (Surge XT and Vital among them), however useful. Code like that belongs in a plug-in's own GPL crates, never in the kit. Papers, textbooks, open standards and permissively licensed code are fine, within their licences; name the source in a comment.
- **No one else's content.** No samples, presets, wavetables, impulse responses, fonts or artwork you didn't make, and no plug-in or SDK binaries.

## Find something to work on

- **Work packages.** Most work is a package a maintainer gives you. Each package has its own branch, `wp/<id>-<slug>`, and its own pull request.
- **Bugs and features.** Open an issue first.
- **Small fixes**, such as a typo or a broken link, can go straight to a pull request.

## Make the change

1. Build to the acceptance criteria. Each criterion is a test or a command whose output you paste into the pull request.
1. Give every feature a headless test path. A feature that can only be tested by clicking isn't done.
1. Register every user-facing action as a command, with a schema, a model-facing description and a permission class, so agents can reach it.
1. Keep to the rules in [AGENTS.md](AGENTS.md): no allocation, lock, or blocking call on an audio thread; the headless crates never depend on `lspk-ui`; no kit crate depends on a plug-in or on Beatlayer.
1. Run the checks. They must pass before you open the pull request.

   ```sh
   cargo xtask check
   ```

   It runs format, clippy with warnings denied, the tests under cargo-nextest and then the doctests, the layer rules, `cargo deny`, and a self-test that proves `deny.toml` still rejects copyleft licences.

## Commit

- Stage files by path. Never use `git add -A` or `git add .`, and check `git status` so build output, `.DS_Store`, and scratch files stay out.
- Start the subject line with the package ID when there is one.
- Sign off every commit, as the next section explains.

## Developer Certificate of Origin

The plug-in kit takes contributions under the [Developer Certificate of Origin](https://developercertificate.org) (DCO), version 1.1, instead of a contributor licence agreement. You keep the copyright in your work. When you sign off a commit, you certify the statements below: in short, that you wrote the change, or otherwise have the right to submit it under the project's open-source licence, and that the record of your contribution is public.

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
git commit -s -m "Fix the smoother's first value"
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

- Open it against `main`. For a work package, title it `<id>: <package title>`.
- Say what the change adds, paste the acceptance evidence and your `cargo xtask check` summary, and list anything that needs a maintainer.
- Update any document whose facts your change alters, in the same pull request.
- A maintainer reviews and merges. Don't merge your own pull request.

## Write in the house style

Write prose, including documents, code comments, commit messages, and pull requests, in plain British English, with sentence-case headings. Address the reader in the second person. `rustfmt` and `clippy` settle the code's formatting, and `cargo xtask check` runs both.

## Licence

The plug-in kit is licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions, and you certify each commit with the Developer Certificate of Origin.
