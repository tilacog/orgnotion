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
fn index_marker_is_parsed_and_keyed_relative_to_vault_root() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/a.org", &org("a", "Ay", "aaa")),
        ("/vault/merged/b.org", &org("b", "Bee", "bbb")),
        ("/vault/merged/index.org", &org("i", "Index", "iii")),
        ("/vault/merged/.INDEX", "index.org\nflat = true\n"),
        ("/vault/normal/c.org", &org("c", "Cee", "ccc")),
    ]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    let dirs: Vec<_> = vault.indexes.keys().collect();
    assert_eq!(dirs, vec![Path::new("merged")]);
    let index = &vault.indexes[Path::new("merged")];
    assert_eq!(index.file, Path::new("/vault/merged/index.org"));
    assert!(index.flat);
    assert_eq!(vault.index_node(Path::new("merged")).unwrap().id, "i");
    assert!(vault.index_node(Path::new("normal")).is_none());
}

#[test]
fn index_marker_lines_may_come_in_any_order_and_flat_may_be_false() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/sub/index.org", &org("i", "Index", "iii")),
        ("/vault/sub/.INDEX", "\nflat = false\n\nindex.org\n"),
    ]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    assert!(!vault.indexes[Path::new("sub")].flat);
}

#[test]
fn index_marker_at_vault_root_is_the_empty_path() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/a.org", &org("a", "Ay", "aaa")),
        ("/vault/.INDEX", "a.org\nflat = true\n"),
    ]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    let dirs: Vec<_> = vault.indexes.keys().collect();
    assert_eq!(dirs, vec![Path::new("")]);
}

#[test]
fn no_marker_means_no_indexes() {
    let fs = InMemoryFileSystem::with_files(&[("/vault/sub/a.org", &org("a", "Ay", "aaa"))]);
    let vault = scan(&fs, Path::new(VAULT)).unwrap();
    assert!(vault.indexes.is_empty());
}

fn scan_index_error(marker_text: &str) -> String {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/sub/a.org", &org("a", "Ay", "aaa")),
        ("/vault/sub/.INDEX", marker_text),
    ]);
    let err = scan(&fs, Path::new(VAULT)).unwrap_err();
    assert!(matches!(err, VaultError::InvalidIndex { .. }), "{err}");
    let rendered = err.to_string();
    assert!(rendered.contains("sub"), "got: {rendered}");
    rendered
}

#[test]
fn index_marker_errors_are_specific() {
    assert!(scan_index_error("a.org\n").contains("missing the \"flat"));
    assert!(scan_index_error("flat = true\n").contains("missing the index node's file path"));
    assert!(scan_index_error("a.org\nflat = maybe\n").contains("must be true or false"));
    assert!(scan_index_error("a.org\nnested = true\n").contains("unknown option"));
    assert!(scan_index_error("a.org\nflat = true\nflat = false\n").contains("more than once"));
    assert!(scan_index_error("a.org\nb.org\nflat = true\n").contains("more than one file path"));
    assert!(scan_index_error("ghost.org\nflat = true\n").contains("not an .org node file"));
}

#[test]
fn index_marker_must_name_a_file_in_its_own_directory() {
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/sub/a.org", &org("a", "Ay", "aaa")),
        ("/vault/sub/deeper/b.org", &org("b", "Bee", "bbb")),
        ("/vault/sub/.INDEX", "deeper/b.org\nflat = true\n"),
    ]);
    let err = scan(&fs, Path::new(VAULT)).unwrap_err();
    assert!(matches!(err, VaultError::InvalidIndex { .. }), "{err}");
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
