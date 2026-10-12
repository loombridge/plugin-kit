//! The private-terms guard behind `cargo xtask private-terms`, which
//! `cargo xtask check` runs too.
//!
//! A maintainer's checkout can keep a list of words that must never appear
//! in this repository at `.git/info/private-terms`, inside git's own folder,
//! which is never committed: one word per line; blank lines and lines
//! starting with `#` are ignored. The guard fails if any tracked file, by its path or its text,
//! contains one of them as a whole word, matched case-sensitively.
//!
//! The list itself stays private, so this file names no term. Without it (an
//! outside contributor's checkout, or CI) the guard says it was skipped and
//! passes.

use std::path::Path;
use std::process::Command;

/// Where the list lives, inside git's common folder (shared by worktrees).
pub const TERMS_FILE: &str = ".git/info/private-terms";

/// The list's path for the checkout at `root`: `info/private-terms` in git's
/// common folder, so every worktree of one clone shares it. `None` outside a
/// git checkout, which has no list.
fn terms_path(root: &Path) -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(root.join(dir).join("info").join("private-terms"))
}

/// Checks every tracked file against the private terms, if the list exists.
pub fn check(root: &Path) -> Result<(), String> {
    let Some(list) = terms_path(root) else {
        eprintln!("skipped: not a git checkout, so there's no {TERMS_FILE}");
        return Ok(());
    };
    let text = match std::fs::read_to_string(&list) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: {TERMS_FILE} isn't here (only maintainers' checkouts have it)");
            return Ok(());
        }
        Err(e) => return Err(format!("couldn't read {TERMS_FILE}: {e}")),
    };
    let terms = parse_terms(&text);
    if terms.is_empty() {
        eprintln!("skipped: {TERMS_FILE} lists no terms");
        return Ok(());
    }

    let mut found = Vec::new();
    let files = tracked_files(root)?;
    for path in &files {
        for term in &terms {
            if contains_word(path, term) {
                found.push(format!("{path}: the path contains \"{term}\""));
            }
        }
        let Ok(bytes) = std::fs::read(root.join(path)) else {
            // Tracked but deleted in the working tree.
            continue;
        };
        // Binary files (fonts, images, audio) aren't text a person writes.
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            for term in &terms {
                if contains_word(line, term) {
                    found.push(format!("{path}:{}: \"{term}\"", number + 1));
                }
            }
        }
    }

    if found.is_empty() {
        eprintln!(
            "{} tracked files checked against {} private terms; none found",
            files.len(),
            terms.len()
        );
        Ok(())
    } else {
        Err(format!(
            "{} private term(s) in tracked files; reword them, or keep the text out of this repository:\n{}",
            found.len(),
            found.join("\n")
        ))
    }
}

/// The terms in the list: one per line, trimmed, without blanks or comments.
fn parse_terms(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(String::from)
        .collect()
}

/// Whether `term` appears in `text` with no letter, digit, or underscore
/// directly before or after it.
fn contains_word(text: &str, term: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(term).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + term.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// The files git tracks, relative to the root.
pub fn tracked_files(root: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("couldn't run git ls-files: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output
        .stdout
        .split(|&b| b == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_words_match_case_sensitively() {
        assert!(contains_word("Ask Quill first", "Quill"));
        assert!(contains_word("Quill's list", "Quill"));
        assert!(contains_word("(Quill, 1 May)", "Quill"));
        assert!(contains_word("Quill", "Quill"));
        assert!(!contains_word("Quills", "Quill"));
        assert!(!contains_word("AppQuill", "Quill"));
        assert!(!contains_word("Quill_name", "Quill"));
        assert!(!contains_word("quill", "Quill"));
        assert!(!contains_word("", "Quill"));
    }

    #[test]
    fn a_later_occurrence_counts_after_an_embedded_one() {
        assert!(contains_word("QuillPen, then Quill", "Quill"));
    }

    #[test]
    fn non_ascii_neighbours_are_letters() {
        assert!(!contains_word("éQuill", "Quill"));
        assert!(contains_word("—Quill—", "Quill"));
    }

    #[test]
    fn the_list_skips_blanks_and_comments() {
        let terms = parse_terms("# private\n\n  Quill \nInkwell\n#Not\n");
        assert_eq!(terms, ["Quill", "Inkwell"]);
    }

    #[test]
    fn a_checkout_without_the_list_passes() {
        let dir = std::env::temp_dir().join(format!("xtask-private-terms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(check(&dir), Ok(()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
