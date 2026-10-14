//! A plug-in's golden list of parameter IDs: every ID it has published,
//! with what it means, kept in its repository and only ever added to, so
//! no host's automation or saved project ever finds an ID meaning
//! something else.
//!
//! The list is [`ParamSpace::id_list`](lspk_params::ParamSpace::id_list)'s
//! text, one ID a line. [`assert_ids`] fails if a line in the kept list is
//! gone or changed (a slot's type parameter may gain choices at the end of
//! its list), and if the plug-in has IDs the kept list hasn't: add those by
//! running the test with [`UPDATE_ENV`] set, and commit the list.

use std::path::Path;

/// The environment variable that lets [`assert_ids`] write new IDs into
/// the kept list. It never lets one be removed or changed.
pub const UPDATE_ENV: &str = "LSPK_IDS_UPDATE";

/// Checks `current` (a space's
/// [`id_list`](lspk_params::ParamSpace::id_list)) against the list kept at
/// `path`.
///
/// # Panics
///
/// If a kept line is gone or changed, or, without [`UPDATE_ENV`], if
/// `current` has lines the kept list hasn't.
pub fn assert_ids(current: &str, path: impl AsRef<Path>) {
    let path = path.as_ref();
    let kept = std::fs::read_to_string(path).unwrap_or_default();
    let gone: Vec<&str> = kept
        .lines()
        .filter(|line| !line.trim().is_empty() && !still_means(line, current))
        .collect();
    assert!(
        gone.is_empty(),
        "these parameter IDs are gone or mean something else now, which breaks hosts' \
         automation and saved projects; give the new meaning a new number instead:\n  {}",
        gone.join("\n  ")
    );
    let new: Vec<&str> = current
        .lines()
        .filter(|line| !kept.lines().any(|k| k == *line))
        .collect();
    if new.is_empty() {
        return;
    }
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::write(path, current)
            .unwrap_or_else(|e| panic!("couldn't write {}: {e}", path.display()));
        return;
    }
    panic!(
        "{} doesn't list these parameter IDs yet; run the test with {UPDATE_ENV}=1 and commit \
         the list:\n  {}",
        path.display(),
        new.join("\n  ")
    );
}

/// Whether the kept `line` still holds in `current`: the same line, or, for
/// a slot's type parameter, the same choices with more after them.
fn still_means(line: &str, current: &str) -> bool {
    match line.split_once(": ") {
        Some((head, choices)) => current.lines().any(|c| {
            c.split_once(": ").is_some_and(|(h, now)| {
                h == head && (now == choices || now.starts_with(&format!("{choices}, ")))
            })
        }),
        None => current.lines().any(|c| c == line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "lspk-test-ids-{}-{name}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn kept_lines_hold_and_choices_only_grow() {
        let now = "0x01000000 osc \"A\" / param \"Level\"\n\
                   0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\", \"Echo\"\n";
        assert!(still_means("0x01000000 osc \"A\" / param \"Level\"", now));
        assert!(still_means(
            "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\"",
            now
        ));
        assert!(!still_means(
            "0x14000000 fx \"1\" / param \"Type\": \"Drive\"",
            now
        ));
        assert!(!still_means("0x01000000 osc \"A\" / param \"Volume\"", now));
    }

    #[test]
    #[should_panic(
        expected = "these parameter IDs are gone or mean something else now, which breaks hosts' automation and saved projects"
    )]
    fn removed_line_panics() {
        let temp = TempFile::new("removed-line");
        let kept = "0x01000000 osc \"A\" / param \"Level\"\n\
                    0x01000001 osc \"A\" / param \"Pan\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x01000000 osc \"A\" / param \"Level\"\n";
        assert_ids(current, temp.path());
    }

    #[test]
    #[should_panic(
        expected = "these parameter IDs are gone or mean something else now, which breaks hosts' automation and saved projects"
    )]
    fn changed_line_panics() {
        let temp = TempFile::new("changed-line");
        let kept = "0x01000000 osc \"A\" / param \"Level\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x01000000 osc \"A\" / param \"Volume\"\n";
        assert_ids(current, temp.path());
    }

    #[test]
    fn choices_may_grow_at_the_end_with_update_env() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::set_var(UPDATE_ENV, "1") };
        let temp = TempFile::new("choices-grow");
        let kept = "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\", \"Echo\"\n";
        assert_ids(current, temp.path());
        assert_eq!(
            std::fs::read_to_string(temp.path()).expect("read updated"),
            current
        );
    }

    #[test]
    #[should_panic(
        expected = "these parameter IDs are gone or mean something else now, which breaks hosts' automation and saved projects"
    )]
    fn choices_shrinking_panics() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::set_var(UPDATE_ENV, "1") };
        let temp = TempFile::new("choices-shrink");
        let kept = "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\", \"Echo\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\"\n";
        assert_ids(current, temp.path());
    }

    #[test]
    #[should_panic(
        expected = "these parameter IDs are gone or mean something else now, which breaks hosts' automation and saved projects"
    )]
    fn choices_changed_panics() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::set_var(UPDATE_ENV, "1") };
        let temp = TempFile::new("choices-changed");
        let kept = "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Drive\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x14000000 fx \"1\" / param \"Type\": \"Off\", \"Echo\"\n";
        assert_ids(current, temp.path());
    }

    #[test]
    #[should_panic(
        expected = "doesn't list these parameter IDs yet; run the test with LSPK_IDS_UPDATE=1"
    )]
    fn new_id_panics_without_update_env() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::remove_var(UPDATE_ENV) };
        let temp = TempFile::new("new-id-panics");
        let kept = "0x01000000 osc \"A\" / param \"Level\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x01000000 osc \"A\" / param \"Level\"\n\
                       0x01000001 osc \"A\" / param \"Pan\"\n";
        assert_ids(current, temp.path());
    }

    #[test]
    fn new_id_updates_when_update_env_is_set() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::set_var(UPDATE_ENV, "1") };
        let temp = TempFile::new("new-id-updates");
        let kept = "0x01000000 osc \"A\" / param \"Level\"\n";
        std::fs::write(temp.path(), kept).expect("write kept");
        let current = "0x01000000 osc \"A\" / param \"Level\"\n\
                       0x01000001 osc \"A\" / param \"Pan\"\n";
        assert_ids(current, temp.path());
        assert_eq!(
            std::fs::read_to_string(temp.path()).expect("read updated"),
            current
        );
    }

    #[test]
    fn empty_kept_list_works() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::remove_var(UPDATE_ENV) };
        let temp = TempFile::new("empty-kept");
        // File does not exist yet:
        assert_ids("", temp.path());

        // File exists and is empty:
        std::fs::write(temp.path(), "").expect("write empty");
        assert_ids("", temp.path());

        // File exists with only blank lines:
        std::fs::write(temp.path(), "\n  \n\t\n").expect("write blank lines");
        assert_ids("", temp.path());
    }

    #[test]
    fn empty_kept_list_with_update_env_populates_ids() {
        // SAFETY: nextest runs each test in its own process, so modifying the
        // environment does not race with other tests.
        unsafe { std::env::set_var(UPDATE_ENV, "1") };
        let temp = TempFile::new("empty-kept-populate");
        let current = "0x01000000 osc \"A\" / param \"Level\"\n";
        assert_ids(current, temp.path());
        assert_eq!(
            std::fs::read_to_string(temp.path()).expect("read file"),
            current
        );
    }
}
