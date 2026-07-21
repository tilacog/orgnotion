//! Unit tests for the org parser (pure logic, no I/O).

use orgnotion::org_parser::{Markup, OrgBlock, Span, parse_node};
use std::path::PathBuf;

fn parse(text: &str) -> orgnotion::org_parser::Node {
    parse_node(&PathBuf::from("test.org"), text).expect("parse should succeed")
}

fn body_spans(node: &orgnotion::org_parser::Node) -> &[Span] {
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => spans,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

#[test]
fn inline_emphasis_becomes_marked_spans_without_delimiters() {
    for (body, text, markup) in [
        ("*bold*", "bold", Markup::Bold),
        ("/italic/", "italic", Markup::Italic),
        ("_underline_", "underline", Markup::Underline),
        ("+gone+", "gone", Markup::Strikethrough),
        ("~code~", "code", Markup::Code),
        ("=verbatim=", "verbatim", Markup::Code),
    ] {
        let node = parse(&format!(
            ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\nx {body} y\n"
        ));
        let spans = body_spans(&node);
        assert_eq!(
            spans,
            &[
                Span::Text("x ".to_string()),
                Span::Marked {
                    text: text.to_string(),
                    markup,
                },
                Span::Text(" y".to_string()),
            ],
            "body: {body}"
        );
    }
}

#[test]
fn emphasis_mixed_with_plain_text_keeps_order_and_drops_markers() {
    // Regression: markup characters used to land literally in the text.
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         *SOL wrapping at driver vs solver.* Mirrors EVM ~ManageNativeToken~ but tangles.\n",
    );
    let spans = body_spans(&node);
    assert_eq!(
        spans,
        &[
            Span::Marked {
                text: "SOL wrapping at driver vs solver.".to_string(),
                markup: Markup::Bold,
            },
            Span::Text(" Mirrors EVM ".to_string()),
            Span::Marked {
                text: "ManageNativeToken".to_string(),
                markup: Markup::Code,
            },
            Span::Text(" but tangles.".to_string()),
        ],
    );
}

#[test]
fn links_inside_emphasis_still_become_link_refs() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         *see [[id:b][the node]] here*\n",
    );
    let spans = body_spans(&node);
    assert_eq!(
        spans,
        &[
            Span::Marked {
                text: "see ".to_string(),
                markup: Markup::Bold,
            },
            Span::LinkRef {
                id: "b".to_string(),
                description: Some("the node".to_string()),
            },
            Span::Marked {
                text: " here".to_string(),
                markup: Markup::Bold,
            },
        ],
    );
}

#[test]
fn extracts_id_and_title_from_drawer() {
    let node = parse(":PROPERTIES:\n:ID: abc-123\n:END:\n#+TITLE: My Node\n\nHello world.\n");
    assert_eq!(node.id, "abc-123");
    assert_eq!(node.title, "My Node");
}

#[test]
fn extracts_id_from_file_keyword() {
    let node = parse("#+ID: kw-id\n#+TITLE: Keyword Style\n\nBody.\n");
    assert_eq!(node.id, "kw-id");
    assert_eq!(node.title, "Keyword Style");
}

#[test]
fn drawer_matching_is_case_insensitive() {
    let node = parse(":properties:\n:id: lower-id\n:end:\n#+title: Lower\n");
    assert_eq!(node.id, "lower-id");
    assert_eq!(node.title, "Lower");
}

#[test]
fn missing_id_is_an_error() {
    let err = parse_node(&PathBuf::from("x.org"), "#+TITLE: No ID\n\nBody\n");
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("x.org"));
}

#[test]
fn title_falls_back_to_first_heading() {
    let node = parse(":PROPERTIES:\n:ID: abc\n:END:\n\n* Heading Text\nBody.\n");
    assert_eq!(node.title, "Heading Text");
}

#[test]
fn title_falls_back_to_filename_stem() {
    let node = parse_node(
        &PathBuf::from("notes/my-note.org"),
        ":PROPERTIES:\n:ID: abc\n:END:\n\nJust a paragraph.\n",
    )
    .unwrap();
    assert_eq!(node.title, "my-note");
}

#[test]
fn extracts_links_with_and_without_descriptions() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         See [[id:b][the other node]] and [[id:c]].\n",
    );
    assert_eq!(node.links, vec!["b".to_string(), "c".to_string()]);
}

#[test]
fn search_suffix_is_stripped_from_link_ids() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         See [[id:b::§1.2.3]] and [[id:c::*Some heading][the section]].\n",
    );
    assert_eq!(node.links, vec!["b".to_string(), "c".to_string()]);
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => {
            assert!(spans.contains(&Span::LinkRef {
                id: "b".to_string(),
                description: None,
            }));
            assert!(spans.contains(&Span::LinkRef {
                id: "c".to_string(),
                description: Some("the section".to_string()),
            }));
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn deduplicates_links_preserving_first_seen_order() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         [[id:c]] then [[id:b]] then [[id:c][again]].\n",
    );
    assert_eq!(node.links, vec!["c".to_string(), "b".to_string()]);
}

#[test]
fn finds_links_in_headings_and_list_items() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         * About [[id:h]]\n- item with [[id:l]]\n1. numbered with [[id:n]]\n",
    );
    assert_eq!(
        node.links,
        vec!["h".to_string(), "l".to_string(), "n".to_string()]
    );
}

#[test]
fn parses_headings_lists_quote_and_code() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         * Section\n\
         - item one\n\
         + item two\n\
         1. first\n\
         2) second\n\
         #+BEGIN_QUOTE\nwise words\n#+END_QUOTE\n\
         #+BEGIN_SRC rust\nfn main() {}\n#+END_SRC\n",
    );
    assert!(matches!(node.blocks[0], OrgBlock::Heading { level: 1, .. }));
    assert!(matches!(node.blocks[1], OrgBlock::BulletItem { .. }));
    assert!(matches!(node.blocks[2], OrgBlock::BulletItem { .. }));
    assert!(matches!(node.blocks[3], OrgBlock::NumberedItem { .. }));
    assert!(matches!(node.blocks[4], OrgBlock::NumberedItem { .. }));
    match &node.blocks[5] {
        OrgBlock::Quote { spans } => {
            assert_eq!(spans, &[Span::Text("wise words".to_string())]);
        }
        other => panic!("expected quote, got {other:?}"),
    }
    match &node.blocks[6] {
        OrgBlock::CodeBlock { language, content } => {
            assert_eq!(language.as_deref(), Some("rust"));
            assert_eq!(content, "fn main() {}");
        }
        other => panic!("expected code block, got {other:?}"),
    }
    assert_eq!(node.blocks.len(), 7);
}

#[test]
fn consecutive_lines_join_into_one_paragraph() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n\nline one\nline two\n\nsecond para\n");
    assert_eq!(node.blocks.len(), 2);
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => {
            assert_eq!(spans, &[Span::Text("line one line two".to_string())]);
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn unterminated_src_block_degrades_to_paragraph() {
    // Without a matching #+END_SRC, orgize does not recognize a source
    // block; the content degrades to a plain paragraph so the run never
    // crashes on malformed input.
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n\n#+BEGIN_SRC sh\necho hi\n");
    assert!(matches!(node.blocks[0], OrgBlock::Paragraph { .. }));
}

#[test]
fn body_drawers_and_keywords_are_skipped() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         * Task\n:LOGBOOK:\nCLOCK: [2026-01-01]\n:END:\n#+filetags: :x:\nreal text\n",
    );
    assert_eq!(node.blocks.len(), 2);
    assert!(matches!(node.blocks[1], OrgBlock::Paragraph { .. }));
}

#[test]
fn heading_level_deeper_than_three_is_preserved_in_parse() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n\n***** Deep\n");
    assert!(matches!(node.blocks[0], OrgBlock::Heading { level: 5, .. }));
}

#[test]
fn link_spans_split_surrounding_text() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n\nbefore [[id:b][mid]] after\n");
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => {
            assert_eq!(
                spans,
                &[
                    Span::Text("before ".to_string()),
                    Span::LinkRef {
                        id: "b".to_string(),
                        description: Some("mid".to_string())
                    },
                    Span::Text(" after".to_string()),
                ]
            );
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn dedicated_targets_pass_through_as_literal_text() {
    // Transformers downstream (anchors_to_bold) rely on `<<...>>` reaching
    // the text spans verbatim.
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n\nsee <<§1.2>> here\n");
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => {
            assert_eq!(spans, &[Span::Text("see <<§1.2>> here".to_string())]);
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn external_links_become_external_link_spans() {
    let node = parse(
        ":PROPERTIES:\n:ID: a\n:END:\n\n\
         See [[https://example.com][site]] and [[https://example.org]] \
         or write to [[mailto:hi@example.com][us]].\n",
    );
    assert!(node.links.is_empty());
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => {
            assert!(spans.contains(&Span::ExternalLink {
                url: "https://example.com".to_string(),
                description: Some("site".to_string()),
            }));
            assert!(spans.contains(&Span::ExternalLink {
                url: "https://example.org".to_string(),
                description: None,
            }));
            assert!(spans.contains(&Span::ExternalLink {
                url: "mailto:hi@example.com".to_string(),
                description: Some("us".to_string()),
            }));
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn non_url_links_are_left_as_plain_text() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n\nSee [[file:notes.org][notes]].\n");
    assert!(node.links.is_empty());
    match &node.blocks[0] {
        OrgBlock::Paragraph { spans } => {
            assert_eq!(spans, &[Span::Text("See notes.".to_string())]);
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn empty_body_yields_no_blocks() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: Empty\n");
    assert!(node.blocks.is_empty());
    assert!(node.links.is_empty());
}

#[test]
fn filetags_are_extracted_without_delimiters() {
    let node =
        parse(":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n#+filetags: :unreviewed:draft:\n\nbody\n");
    assert_eq!(node.tags, vec!["unreviewed", "draft"]);
    assert!(node.has_tag("unreviewed"));
    assert!(node.has_tag("UNREVIEWED"));
    assert!(!node.has_tag("reviewed"));
}

#[test]
fn space_separated_filetags_are_extracted() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n#+FILETAGS: alpha beta\n\nbody\n");
    assert_eq!(node.tags, vec!["alpha", "beta"]);
}

#[test]
fn missing_filetags_yields_no_tags() {
    let node = parse(":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\nbody\n");
    assert!(node.tags.is_empty());
    assert!(!node.has_tag("unreviewed"));
}
