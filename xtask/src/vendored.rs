//! The check behind `cargo xtask vendored`, which `cargo xtask check` runs
//! too: cargo-deny checks the licences of Rust dependencies, and this does
//! the same for the C and C++ code vendored in the repository (in folders
//! named `vendor`).
//!
//! Every vendored component is listed in [`VENDORED`] with its licence and
//! the file that states it. The check fails if a listed licence file is
//! missing or no longer says what it did, if a licence isn't one deny.toml
//! allows (or the public domain), if a vendored file belongs to no listed
//! component, or if a vendored file's own text claims stricter terms
//! ([`RESTRICTIVE`]).

use std::path::Path;

/// A vendored component.
pub struct Vendored {
    /// Its folder, from the repository's root.
    pub path: &'static str,
    /// Its licence, as an SPDX identifier, or [`PUBLIC_DOMAIN`].
    pub licence: &'static str,
    /// The file stating the licence, inside `path`.
    pub file: &'static str,
    /// Words that file contains, which identify the licence.
    pub phrase: &'static str,
}

/// Words that mark a file under terms of its own, stricter than its
/// component's licence, as an SDK file left with an older header can be.
pub const RESTRICTIVE: &[&str] = &[
    "may not be distributed in parts",
    "prior written agreement",
    "All rights reserved. Confidential",
];

/// A component placed in the public domain, which has no SPDX identifier.
pub const PUBLIC_DOMAIN: &str = "public domain";

/// Every vendored component. Keep in step with the table in
/// `crates/clap-wrapper/src/lib.rs`.
pub const VENDORED: &[Vendored] = &[
    Vendored {
        path: "crates/clap-wrapper/vendor/clap-wrapper",
        licence: "MIT",
        file: "LICENSE",
        phrase: "MIT License",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/clap-wrapper/libs/fmt",
        licence: "MIT",
        file: "fmt/format.h",
        phrase: "Permission is hereby granted, free of charge",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/clap-wrapper/libs/psl",
        licence: PUBLIC_DOMAIN,
        file: "presonus-plugin-extensions/ipslgainreduction.h",
        phrase: "placed in the PUBLIC DOMAIN",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/clap",
        licence: "MIT",
        file: "LICENSE",
        phrase: "MIT License",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/vst3sdk/base",
        licence: "MIT",
        file: "LICENSE.txt",
        phrase: "MIT License",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/vst3sdk/pluginterfaces",
        licence: "MIT",
        file: "LICENSE.txt",
        phrase: "MIT License",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/vst3sdk/public.sdk",
        licence: "MIT",
        file: "LICENSE.txt",
        phrase: "MIT License",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/AudioUnitSDK",
        licence: "Apache-2.0",
        file: "LICENSE.txt",
        phrase: "Apache License",
    },
    Vendored {
        path: "crates/clap-wrapper/vendor/filesystem",
        licence: "MIT",
        file: "LICENSE",
        phrase: "Permission is hereby granted, free of charge",
    },
];

/// Checks every vendored component against [`VENDORED`] and deny.toml.
pub fn check(root: &Path, tracked: &[String]) -> Result<(), String> {
    let deny = std::fs::read_to_string(root.join("deny.toml"))
        .map_err(|e| format!("couldn't read deny.toml: {e}"))?;
    let allowed = allowed_licences(&deny);
    let mut problems = Vec::new();
    for component in VENDORED {
        let file = root.join(component.path).join(component.file);
        match std::fs::read_to_string(&file) {
            Ok(text) if text.contains(component.phrase) => {}
            Ok(_) => problems.push(format!(
                "{} no longer says \"{}\": check its licence",
                file.display(),
                component.phrase
            )),
            Err(e) => problems.push(format!("{}: {e}", file.display())),
        }
        if component.licence != PUBLIC_DOMAIN && !allowed.iter().any(|l| l == component.licence) {
            problems.push(format!(
                "{} is {}, which deny.toml doesn't allow",
                component.path, component.licence
            ));
        }
    }
    problems.extend(unlisted(tracked));
    problems.extend(restrictive(root, tracked));
    if problems.is_empty() {
        eprintln!(
            "{} vendored components, each under a licence the kit allows",
            VENDORED.len()
        );
        Ok(())
    } else {
        Err(format!("vendored code:\n  {}", problems.join("\n  ")))
    }
}

/// Each tracked file in a `vendor` folder that belongs to no listed
/// component, one line each.
fn unlisted(tracked: &[String]) -> Vec<String> {
    tracked
        .iter()
        .filter(|path| path.split('/').any(|part| part == "vendor"))
        .filter(|path| {
            !VENDORED
                .iter()
                .any(|c| path.starts_with(&format!("{}/", c.path)))
        })
        .map(|path| format!("{path} is vendored but belongs to no component in VENDORED"))
        .collect()
}

/// Each tracked vendored file whose text carries terms stricter than its
/// component's licence, one line each.
fn restrictive(root: &Path, tracked: &[String]) -> Vec<String> {
    tracked
        .iter()
        .filter(|path| path.split('/').any(|part| part == "vendor"))
        .filter_map(|path| {
            let text = std::fs::read(root.join(path)).ok()?;
            let text = String::from_utf8_lossy(&text);
            let phrase = RESTRICTIVE.iter().find(|p| text.contains(**p))?;
            Some(format!(
                "{path} says \"{phrase}\": its terms may not be its component's licence"
            ))
        })
        .collect()
}

/// The licences in deny.toml's `allow` list.
fn allowed_licences(deny: &str) -> Vec<String> {
    let Some(start) = deny.find("\nallow = [") else {
        return Vec::new();
    };
    let list = &deny[start..];
    let list = &list[..list.find(']').unwrap_or(list.len())];
    list.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix('"')?;
            Some(line[..line.find('"')?].to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allow_list_is_read_from_deny_toml() {
        let deny = "[licenses]\nallow = [\n    \"MIT\",\n    \"Apache-2.0\",\n]\nexceptions = []\n";
        assert_eq!(allowed_licences(deny), ["MIT", "Apache-2.0"]);
    }

    #[test]
    fn a_vendored_file_outside_every_component_is_caught() {
        let tracked = [
            "crates/clap-wrapper/vendor/clap/LICENSE".to_owned(),
            "crates/clap-wrapper/vendor/other/LICENSE".to_owned(),
            "crates/clap-wrapper/src/lib.rs".to_owned(),
        ];
        let problems = unlisted(&tracked);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("vendor/other/LICENSE"));
    }
}
