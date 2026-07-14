//! Unit tests for pre- and post-validation, using the in-memory Notion
//! fake for the post-validation read-back path.

mod common;

use common::FakeNotion;
use notionrs_types::object::block::{
    Block, bulleted_list_item::BulletedListItemBlock, paragraph::ParagraphBlock,
};
use notionrs_types::object::rich_text::RichText;
use orgnotion::converter::{LinkTarget, page_mention, text_run};
use orgnotion::org_parser::Node;
use orgnotion::ports::{AppendPosition, NotionApi};
use orgnotion::validate::{post_validate, pre_validate};
use orgnotion::vault::Vault;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

fn paragraph(rich_text: Vec<RichText>) -> Block {
    Block::Paragraph {
        paragraph: ParagraphBlock::default().rich_text(rich_text),
    }
}

fn node(id: &str, links: &[&str]) -> Node {
    Node {
        id: id.to_string(),
        file_path: PathBuf::from(format!("{id}.org")),
        title: id.to_uppercase(),
        links: links.iter().map(|s| (*s).to_string()).collect(),
        tags: vec![],
        blocks: vec![],
    }
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, LinkTarget> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), LinkTarget::Page((*v).to_string())))
        .collect()
}

#[test]
fn pre_validate_passes_when_all_links_resolve() {
    let vault = Vault {
        nodes: vec![node("a", &["b"]), node("b", &["a"])],
        indexes: BTreeMap::default(),
    };
    assert!(pre_validate(&vault).is_ok());
}

#[test]
fn pre_validate_collects_every_broken_link() {
    let vault = Vault {
        nodes: vec![node("a", &["ghost-1", "b"]), node("b", &["ghost-2"])],
        indexes: BTreeMap::default(),
    };
    let broken = pre_validate(&vault).unwrap_err();
    assert_eq!(broken.len(), 2);
    assert_eq!(broken[0].target_id, "ghost-1");
    assert_eq!(broken[0].source_file, "a.org");
    assert_eq!(broken[1].target_id, "ghost-2");
    let rendered = broken[0].to_string();
    assert!(rendered.contains("a.org"), "got: {rendered}");
    assert!(rendered.contains("ghost-1"), "got: {rendered}");
}

#[test]
fn pre_validate_of_empty_vault_passes() {
    let vault = Vault {
        nodes: vec![],
        indexes: BTreeMap::default(),
    };
    assert!(pre_validate(&vault).is_ok());
}

#[tokio::test]
async fn post_validate_passes_when_mentions_landed() {
    let notion = FakeNotion::new();
    notion
        .append_children(
            "page-a",
            &[paragraph(vec![text_run("see "), page_mention("page-b")])],
            AppendPosition::End,
        )
        .await
        .unwrap();

    let result = post_validate(
        &notion,
        &node("a", &["b"]),
        &map(&[("a", "page-a"), ("b", "page-b")]),
    )
    .await
    .unwrap();
    assert!(result.passed());
}

#[tokio::test]
async fn post_validate_flags_missing_mention() {
    let notion = FakeNotion::new();
    notion
        .append_children("page-a", &[paragraph(vec![text_run("no mention here")])], AppendPosition::End)
        .await
        .unwrap();

    let result = post_validate(
        &notion,
        &node("a", &["b"]),
        &map(&[("a", "page-a"), ("b", "page-b")]),
    )
    .await
    .unwrap();
    assert!(!result.passed());
    assert_eq!(result.missing, vec!["b".to_string()]);
}

#[tokio::test]
async fn post_validate_flags_mention_of_wrong_page() {
    let notion = FakeNotion::new();
    notion
        .append_children("page-a", &[paragraph(vec![page_mention("page-INTRUDER")])], AppendPosition::End)
        .await
        .unwrap();

    let result = post_validate(
        &notion,
        &node("a", &["b"]),
        &map(&[("a", "page-a"), ("b", "page-b")]),
    )
    .await
    .unwrap();
    assert_eq!(result.missing, vec!["b".to_string()]);
}

#[tokio::test]
async fn post_validate_follows_pagination_cursors() {
    let mut notion = FakeNotion::new();
    notion.list_page_size = 1; // force one block per page of results
    let blocks: Vec<_> = ["x", "y"]
        .iter()
        .map(|t| paragraph(vec![text_run(t)]))
        .chain(std::iter::once(paragraph(vec![page_mention("page-b")])))
        .collect();
    notion.append_children("page-a", &blocks, AppendPosition::End).await.unwrap();

    let result = post_validate(
        &notion,
        &node("a", &["b"]),
        &map(&[("a", "page-a"), ("b", "page-b")]),
    )
    .await
    .unwrap();
    assert!(result.passed());
    // 3 blocks at page_size 1 → 3 list calls, cursor advancing.
    let calls = notion.list_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].1, None);
    assert_eq!(calls[1].1, Some("1".to_string()));
    assert_eq!(calls[2].1, Some("2".to_string()));
}

#[tokio::test]
async fn post_validate_finds_mentions_in_nested_blocks() {
    let notion = FakeNotion::new();
    // A parent block whose mention lives one level down: the fake derives
    // child IDs deterministically and reports `has_children` for any block
    // that has content appended under its derived ID.
    notion
        .append_children(
            "page-a",
            &[Block::BulletedListItem {
                bulleted_list_item: BulletedListItemBlock::default()
                    .rich_text(vec![text_run("outer")]),
            }],
            AppendPosition::End,
        )
        .await
        .unwrap();
    notion
        .append_children(
            &FakeNotion::child_id("page-a", 0),
            &[paragraph(vec![page_mention("page-b")])],
            AppendPosition::End,
        )
        .await
        .unwrap();

    let result = post_validate(
        &notion,
        &node("a", &["b"]),
        &map(&[("a", "page-a"), ("b", "page-b")]),
    )
    .await
    .unwrap();
    assert!(result.passed());
}

#[tokio::test]
async fn post_validate_without_page_mapping_reports_all_links_missing() {
    let notion = FakeNotion::new();
    let result = post_validate(&notion, &node("a", &["b", "c"]), &map(&[]))
        .await
        .unwrap();
    assert_eq!(result.missing, vec!["b".to_string(), "c".to_string()]);
}
