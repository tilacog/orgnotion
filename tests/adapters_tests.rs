//! Tests for the filesystem/clock/env adapters and the CLI failure
//! rendering.

use orgnotion::adapters::{ProcessEnv, RealFileSystem, SystemClock};
use orgnotion::cli::{exit_code_for, render_failure};
use orgnotion::ports::{Clock, Env, FileSystem, FsError, NotionError};
use orgnotion::run::RunError;
use orgnotion::validate::{BrokenLink, PostValidationResult};
use std::fs;
use std::path::PathBuf;

/// A unique throwaway directory under the system temp dir, removed on
/// drop.
struct TempVault {
    root: PathBuf,
}

impl TempVault {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("orgnotion-test-{name}-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

impl Drop for TempVault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn real_fs_lists_only_org_files_recursively() {
    let vault = TempVault::new("list");
    vault.write("a.org", "A");
    vault.write("sub/deep/b.org", "B");
    vault.write("ignored.txt", "no");
    vault.write("sub/also.md", "no");

    let mut files = RealFileSystem.list_org_files(&vault.root).unwrap();
    files.sort();
    let names: Vec<_> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&vault.root)
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert_eq!(names, vec!["a.org", "sub/deep/b.org"]);
}

#[test]
fn real_fs_reads_file_contents() {
    let vault = TempVault::new("read");
    vault.write("a.org", "file body");
    let text = RealFileSystem
        .read_to_string(&vault.root.join("a.org"))
        .unwrap();
    assert_eq!(text, "file body");
}

#[test]
fn real_fs_file_exists_only_for_regular_files() {
    let vault = TempVault::new("exists");
    vault.write("sub/.INDEX", "");
    assert!(RealFileSystem.file_exists(&vault.root.join("sub/.INDEX")));
    assert!(!RealFileSystem.file_exists(&vault.root.join("sub"))); // a directory
    assert!(!RealFileSystem.file_exists(&vault.root.join("missing")));
}

#[test]
fn real_fs_missing_dir_is_not_a_directory_error() {
    let err = RealFileSystem
        .list_org_files(&PathBuf::from("/definitely/not/here"))
        .unwrap_err();
    assert!(matches!(err, FsError::NotADirectory(_)));
}

#[test]
fn real_fs_missing_file_is_io_error_with_path() {
    let err = RealFileSystem
        .read_to_string(&PathBuf::from("/definitely/not/here.org"))
        .unwrap_err();
    assert!(err.to_string().contains("here.org"));
}

#[test]
fn system_clock_produces_iso8601_utc_and_monotonic_progress() {
    let clock = SystemClock::new();
    let now = clock.now_iso8601();
    // e.g. 2026-07-12T14:03:00Z
    assert!(now.ends_with('Z'), "got: {now}");
    assert!(now.contains('T'), "got: {now}");
    let a = clock.monotonic();
    let b = clock.monotonic();
    assert!(b >= a);
}

#[test]
fn process_env_reads_real_variables() {
    // PATH is guaranteed in any sane test environment.
    assert!(ProcessEnv.var("PATH").is_some());
    assert!(ProcessEnv.var("ORGNOTION_SURELY_UNSET_VAR").is_none());
}

#[test]
fn exit_codes_map_per_failure_kind() {
    let pre = RunError::PreValidation(vec![]);
    let post = RunError::PostValidation {
        root_url: "u".to_string(),
        failures: vec![],
    };
    let api = RunError::Api {
        context: "c".to_string(),
        source: NotionError::Transport("t".to_string()),
        root_url: None,
    };
    assert_eq!(exit_code_for(&pre), 2);
    assert_eq!(exit_code_for(&post), 3);
    assert_eq!(exit_code_for(&api), 1);
    assert_eq!(exit_code_for(&RunError::MissingToken), 1);
}

#[test]
fn pre_validation_failure_lists_every_broken_link() {
    let error = RunError::PreValidation(vec![
        BrokenLink {
            source_node_id: "a".to_string(),
            source_file: "a.org".to_string(),
            target_id: "ghost-1".to_string(),
        },
        BrokenLink {
            source_node_id: "b".to_string(),
            source_file: "b.org".to_string(),
            target_id: "ghost-2".to_string(),
        },
    ]);
    let rendered = render_failure(&error);
    assert!(rendered.contains("ghost-1"), "got: {rendered}");
    assert!(rendered.contains("ghost-2"), "got: {rendered}");
    assert!(rendered.contains("nothing was written"), "got: {rendered}");
}

#[test]
fn post_validation_failure_warns_about_partial_snapshot() {
    let error = RunError::PostValidation {
        root_url: "https://www.notion.so/root".to_string(),
        failures: vec![PostValidationResult {
            node_id: "a".to_string(),
            missing: vec!["b".to_string()],
        }],
    };
    let rendered = render_failure(&error);
    assert!(rendered.contains("node a"), "got: {rendered}");
    assert!(
        rendered.contains("NOT automatically deleted"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("https://www.notion.so/root"),
        "got: {rendered}"
    );
}

#[test]
fn api_failure_with_root_warns_and_without_root_does_not() {
    let with_root = RunError::Api {
        context: "writing content".to_string(),
        source: NotionError::Transport("t".to_string()),
        root_url: Some("https://www.notion.so/root".to_string()),
    };
    assert!(render_failure(&with_root).contains("NOT automatically deleted"));

    let without_root = RunError::Api {
        context: "creating root".to_string(),
        source: NotionError::Transport("t".to_string()),
        root_url: None,
    };
    assert!(!render_failure(&without_root).contains("NOT automatically deleted"));
}
