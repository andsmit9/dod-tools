//! Shared helpers for this crate's own unit tests.
//!
//! Only [`Scratch`] so far, which exists because the old pattern leaked a
//! directory into `%TEMP%` on every `cargo test` run:
//!
//! ```ignore
//! let dir = std::env::temp_dir().join(format!("dod_thing_{}", std::process::id()));
//! let _ = std::fs::remove_dir_all(&dir);   // clears a *previous* run
//! std::fs::create_dir_all(&dir).unwrap();
//! // ... and nothing removes it
//! ```
//!
//! Clearing on entry is right — it is what makes each test deterministic — but
//! it only ever reclaims the directory belonging to a run with the *same* pid.
//! The next `cargo test` gets a different pid and a fresh directory, and the
//! old one is orphaned for good. 10,422 of them had accumulated by the time
//! anyone looked ([issue #253]).
//!
//! A trailing `remove_dir_all` would not have been enough either: a test that
//! panics never reaches it, and a panicking test is exactly the one that leaves
//! the most behind. Hence a guard with a `Drop`, which the unwinder runs.
//!
//! [issue #253]: https://github.com/ccoventry/dod-tools/issues/253

use std::path::{Path, PathBuf};

/// A temporary directory that empties itself on the way in and removes itself
/// on the way out.
///
/// Hold it for the whole test — `let _dir = Scratch::new(...)` rather than
/// `let _ = Scratch::new(...)`, which drops it immediately and deletes the
/// directory before the test has used it.
pub(crate) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// `<temp>/dod_<tag>_<pid>`, emptied and created.
    ///
    /// The pid keeps two concurrently-running test binaries apart; within one
    /// binary the tag has to be unique, which it already was.
    pub(crate) fn new(tag: impl std::fmt::Display) -> Self {
        Self::at(std::env::temp_dir().join(format!("dod_{tag}_{}", std::process::id())))
    }

    /// Takes over an arbitrary path, for the handful of tests that need an
    /// exact one. Creates it if it does not exist.
    pub(crate) fn at(path: PathBuf) -> Self {
        // Clearing on entry stays, and is not redundant with the `Drop`: it is
        // what makes a directory left behind by a *previous* build -- one from
        // before this guard existed, or from a hard-killed test process --
        // recoverable rather than permanently poisoning the next run.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("could not create the scratch directory");
        Self { path }
    }

    /// A path that is deliberately *not* created, for a test that needs a
    /// name nothing exists at. Still removed on drop, in case the code under
    /// test creates it.
    pub(crate) fn absent(tag: impl std::fmt::Display) -> Self {
        let path = std::env::temp_dir().join(format!("dod_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Ignored rather than unwrapped: a `Drop` that panics while the thread
        // is already unwinding from a failed assertion aborts the process, and
        // would replace the assertion's message with a much less useful one.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// So a `Scratch` can be passed anywhere a `&Path` is wanted, exactly as the
/// `PathBuf` it replaced could. `PathBuf` itself derefs to `Path`, so this is
/// the same relationship, and it is what keeps the call sites unchanged.
impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_directory_is_created_and_then_removed() {
        let path;
        {
            let dir = Scratch::new("test_support_lifecycle");
            path = dir.path().to_path_buf();
            assert!(path.is_dir(), "the directory should exist while the guard is held");
            std::fs::write(dir.join("file"), b"x").unwrap();
        }
        assert!(!path.exists(), "the guard should have removed the tree, contents and all");
    }

    #[test]
    fn a_panicking_test_still_cleans_up() {
        // The whole reason this is a guard rather than a trailing statement:
        // the test that leaves the most behind is the one that fails.
        let path = std::env::temp_dir()
            .join(format!("dod_test_support_panic_{}", std::process::id()));
        let caught = std::panic::catch_unwind(|| {
            let _dir = Scratch::at(path.clone());
            panic!("as a failing test would");
        });
        assert!(caught.is_err(), "the panic should have propagated");
        assert!(!path.exists(), "unwinding should still have run the Drop");
    }

    #[test]
    fn entry_clearing_survives_a_directory_left_by_an_earlier_run() {
        let path = std::env::temp_dir()
            .join(format!("dod_test_support_stale_{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("left_over"), b"from a previous run").unwrap();

        let dir = Scratch::at(path.clone());
        assert!(dir.is_dir());
        assert!(
            !path.join("left_over").exists(),
            "a directory from a previous run must be emptied, not inherited"
        );
    }

    #[test]
    fn an_absent_scratch_names_a_path_that_does_not_exist() {
        let dir = Scratch::absent("test_support_absent");
        assert!(!dir.exists(), "`absent` must not create the directory");
    }
}
