//! Unit tests for vault scanning, using the in-memory filesystem fake.

mod common;

use common::InMemoryFileSystem;
use orgnotion::vault::{VaultError, scan};
use std::path::Path;

const VAULT: &str = "/vault";

fn org(id: &str, title: &str, body: &str) -> String {
    format!(":PROPERTIES:\n:ID: {id}\n:END:\n#+TITLE: {title}\n\n{body}\n")
}

#[test]
fn scans_and_parses_all_org_files_in_sorted_order() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/z-last.org", &org("z", "Zed", "zzz")),
        ("/vault/a-first.org", &org("a", "Ay", "aaa")),
        ("/vault/sub/m-mid.org", &org("m", "Em", "mmm")),
        ("/vault/notes.txt", "not an org file"),
    ]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    let ids: Vec<_> = vault.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "m", "z"]);
}

#[test]
fn continuous_marker_flags_directories_relative_to_vault_root() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/a.org", &org("a", "Ay", "aaa")),
        ("/vault/merged/b.org", &org("b", "Bee", "bbb")),
        ("/vault/merged/.CONTINUOUS", ""),
        ("/vault/normal/c.org", &org("c", "Cee", "ccc")),
    ]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    let dirs: Vec<_> = vault.continuous_dirs.iter().collect();
    assert_eq!(dirs, vec![Path::new("merged")]);
}

#[test]
fn continuous_marker_at_vault_root_is_the_empty_path() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/a.org", &org("a", "Ay", "aaa")),
        ("/vault/.CONTINUOUS", ""),
    ]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    let dirs: Vec<_> = vault.continuous_dirs.iter().collect();
    assert_eq!(dirs, vec![Path::new("")]);
}

#[test]
fn no_marker_means_no_continuous_dirs() {
    let fs = InMemoryFileSystem::with_files(&[("/vault/sub/a.org", &org("a", "Ay", "aaa"))]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    assert!(vault.continuous_dirs.is_empty());
}

#[test]
fn id_index_maps_ids_to_nodes() {
    let fs = InMemoryFileSystem::with_files(&[("/vault/a.org", &org("a", "Ay", "body"))]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    let index = vault.id_index();
    assert_eq!(index["a"].title, "Ay");
}

#[test]
fn missing_directory_errors() {
    let fs = InMemoryFileSystem::default();
    let err = scan(&fs, Path::new("/nowhere")).unwrap_err();
    assert!(matches!(err, VaultError::Fs(_)));
}

#[test]
fn unparsable_file_errors_with_its_path() {
    let fs = InMemoryFileSystem::with_files(&[("/vault/broken.org", "no id here\n")]);
    let err = scan(&fs, Path::new(VAULT)).unwrap_err();
    assert!(err.to_string().contains("broken.org"));
}

#[test]
fn duplicate_ids_across_files_error() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/one.org", &org("dup", "One", "x")),
        ("/vault/two.org", &org("dup", "Two", "y")),
    ]);
    let err = scan(&fs, Path::new(VAULT)).unwrap_err();
    match err {
        VaultError::DuplicateId { id, first, second } => {
            assert_eq!(id, "dup");
            assert!(first.contains("one.org"));
            assert!(second.contains("two.org"));
        }
        other => panic!("expected DuplicateId, got {other}"),
    }
}
