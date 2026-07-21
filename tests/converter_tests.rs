//! Unit tests for org → Notion block conversion (pure logic, no I/O).
//!
//! Blocks are typed (`notionrs_types`); assertions go through
//! `serde_json::to_value` where checking the serialized wire shape is the
//! point, and through pattern matching where the variant is the point.

use notionrs_types::object::block::Block;
use orgnotion::converter::{Converter, LinkTarget, convert_blocks, page_mention, text_run};
use orgnotion::org_parser::{OrgBlock, Span};
use serde_json::Value;
use std::collections::HashMap;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, LinkTarget> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), LinkTarget::Page((*v).to_string())))
        .collect()
}

fn text_spans(s: &str) -> Vec<Span> {
    vec![Span::Text(s.to_string())]
}

/// Serialized JSON view of converted blocks, for wire-shape assertions.
fn json(blocks: &[Block]) -> Value {
    serde_json::to_value(blocks).expect("blocks serialize")
}

#[test]
fn marked_spans_carry_notion_annotations() {
    use orgnotion::org_parser::Markup;
    for (markup, field) in [
        (Markup::Bold, "bold"),
        (Markup::Italic, "italic"),
        (Markup::Underline, "underline"),
        (Markup::Strikethrough, "strikethrough"),
        (Markup::Code, "code"),
    ] {
        let blocks = convert_blocks(
            &[OrgBlock::Paragraph {
                spans: vec![Span::Marked {
                    text: "styled".to_string(),
                    markup,
                }],
            }],
            &map(&[]),
        );
        let run = &json(&blocks)[0]["paragraph"]["rich_text"][0];
        assert_eq!(run["text"]["content"], "styled", "markup: {field}");
        assert_eq!(run["annotations"][field], true, "markup: {field}");
    }
}

#[test]
fn paragraph_converts_to_notion_paragraph() {
    let blocks = convert_blocks(
        &[OrgBlock::Paragraph {
            spans: text_spans("hello"),
        }],
        &map(&[]),
    );
    assert!(matches!(blocks[0], Block::Paragraph { .. }));
    let j = json(&blocks);
    assert_eq!(j[0]["type"], "paragraph");
    assert_eq!(
        j[0]["paragraph"]["rich_text"][0]["text"]["content"],
        "hello"
    );
}

#[test]
fn heading_levels_map_and_clamp() {
    for (level, expected) in [
        (1u8, "heading_1"),
        (2, "heading_2"),
        (3, "heading_3"),
        (7, "heading_3"),
    ] {
        let blocks = convert_blocks(
            &[OrgBlock::Heading {
                level,
                spans: text_spans("h"),
            }],
            &map(&[]),
        );
        let j = json(&blocks);
        assert_eq!(j[0]["type"], expected, "org level {level}");
        assert!(j[0][expected]["rich_text"].is_array());
    }
}

#[test]
fn list_items_convert() {
    let blocks = convert_blocks(
        &[
            OrgBlock::BulletItem {
                spans: text_spans("b"),
            },
            OrgBlock::NumberedItem {
                spans: text_spans("n"),
            },
        ],
        &map(&[]),
    );
    assert!(matches!(blocks[0], Block::BulletedListItem { .. }));
    assert!(matches!(blocks[1], Block::NumberedListItem { .. }));
}

#[test]
fn quote_converts() {
    let blocks = convert_blocks(
        &[OrgBlock::Quote {
            spans: text_spans("wise"),
        }],
        &map(&[]),
    );
    let j = json(&blocks);
    assert_eq!(j[0]["type"], "quote");
    assert_eq!(j[0]["quote"]["rich_text"][0]["text"]["content"], "wise");
}

#[test]
fn code_block_keeps_language_and_content() {
    let blocks = convert_blocks(
        &[OrgBlock::CodeBlock {
            language: Some("rust".to_string()),
            content: "fn main() {}".to_string(),
        }],
        &map(&[]),
    );
    let j = json(&blocks);
    assert_eq!(j[0]["type"], "code");
    assert_eq!(j[0]["code"]["language"], "rust");
    assert_eq!(
        j[0]["code"]["rich_text"][0]["text"]["content"],
        "fn main() {}"
    );
}

#[test]
fn code_language_aliases_and_unknowns_normalize() {
    for (input, expected) in [
        (Some("sh"), "shell"),
        (Some("PYTHON"), "python"),
        (Some("emacs-lisp"), "lisp"),
        (Some("klingon"), "plain text"),
        (None, "plain text"),
    ] {
        let blocks = convert_blocks(
            &[OrgBlock::CodeBlock {
                language: input.map(String::from),
                content: String::new(),
            }],
            &map(&[]),
        );
        assert_eq!(
            json(&blocks)[0]["code"]["language"],
            expected,
            "input {input:?}"
        );
    }
}

#[test]
fn resolved_link_becomes_page_mention() {
    let blocks = convert_blocks(
        &[OrgBlock::Paragraph {
            spans: vec![
                Span::Text("see ".to_string()),
                Span::LinkRef {
                    id: "org-b".to_string(),
                    description: Some("other".to_string()),
                },
            ],
        }],
        &map(&[("org-b", "notion-b")]),
    );
    let j = json(&blocks);
    let rich = j[0]["paragraph"]["rich_text"].as_array().unwrap();
    assert_eq!(rich[0]["type"], "text");
    assert_eq!(rich[1]["type"], "mention");
    assert_eq!(rich[1]["mention"]["type"], "page");
    assert_eq!(rich[1]["mention"]["page"]["id"], "notion-b");
}

#[test]
fn unresolved_link_degrades_to_plain_text() {
    let blocks = convert_blocks(
        &[OrgBlock::Paragraph {
            spans: vec![Span::LinkRef {
                id: "missing".to_string(),
                description: None,
            }],
        }],
        &map(&[]),
    );
    let j = json(&blocks);
    let rich = &j[0]["paragraph"]["rich_text"][0];
    assert_eq!(rich["type"], "text");
    assert_eq!(rich["text"]["content"], "[[id:missing]]");
}

#[test]
fn unresolved_link_prefers_its_description() {
    let blocks = convert_blocks(
        &[OrgBlock::Paragraph {
            spans: vec![Span::LinkRef {
                id: "missing".to_string(),
                description: Some("nice name".to_string()),
            }],
        }],
        &map(&[]),
    );
    assert_eq!(
        json(&blocks)[0]["paragraph"]["rich_text"][0]["text"]["content"],
        "nice name"
    );
}

#[test]
fn external_link_becomes_inline_link_run() {
    let blocks = convert_blocks(
        &[OrgBlock::Paragraph {
            spans: vec![
                Span::ExternalLink {
                    url: "https://example.com".to_string(),
                    description: Some("site".to_string()),
                },
                Span::ExternalLink {
                    url: "https://example.org".to_string(),
                    description: None,
                },
            ],
        }],
        &map(&[]),
    );
    let j = json(&blocks);
    let rich = j[0]["paragraph"]["rich_text"].as_array().unwrap();
    assert_eq!(rich[0]["type"], "text");
    assert_eq!(rich[0]["text"]["content"], "site");
    assert_eq!(rich[0]["text"]["link"]["url"], "https://example.com");
    // A bare URL links to itself, with the URL as display text.
    assert_eq!(rich[1]["text"]["content"], "https://example.org");
    assert_eq!(rich[1]["text"]["link"]["url"], "https://example.org");
}

#[test]
fn long_text_is_chunked_to_notion_limit() {
    let long = "a".repeat(4500);
    let blocks = convert_blocks(
        &[OrgBlock::Paragraph {
            spans: text_spans(&long),
        }],
        &map(&[]),
    );
    let j = json(&blocks);
    let rich = j[0]["paragraph"]["rich_text"].as_array().unwrap();
    assert_eq!(rich.len(), 3); // 2000 + 2000 + 500
    let total: usize = rich
        .iter()
        .map(|r| r["text"]["content"].as_str().unwrap().len())
        .sum();
    assert_eq!(total, 4500);
}

#[test]
fn empty_paragraph_still_produces_valid_rich_text() {
    let blocks = convert_blocks(&[OrgBlock::Paragraph { spans: vec![] }], &map(&[]));
    let j = json(&blocks);
    let rich = j[0]["paragraph"]["rich_text"].as_array().unwrap();
    assert_eq!(rich.len(), 1);
    assert_eq!(rich[0]["text"]["content"], "");
}

#[test]
fn converter_without_transformers_matches_convert_blocks() {
    let input = [OrgBlock::Paragraph {
        spans: text_spans("same"),
    }];
    assert_eq!(
        json(&Converter::new().convert(&input, &map(&[]))),
        json(&convert_blocks(&input, &map(&[])))
    );
}

#[test]
fn org_transformers_rewrite_blocks_before_conversion() {
    let converter = Converter::new()
        // Steps run in registration order: first drop quotes...
        .with_org_transformer(|blocks| {
            blocks
                .into_iter()
                .filter(|b| !matches!(b, OrgBlock::Quote { .. }))
                .collect()
        })
        // ...then prepend a heading to whatever survived.
        .with_org_transformer(|mut blocks| {
            blocks.insert(
                0,
                OrgBlock::Heading {
                    level: 1,
                    spans: text_spans("added"),
                },
            );
            blocks
        });
    let blocks = converter.convert(
        &[
            OrgBlock::Quote {
                spans: text_spans("dropped"),
            },
            OrgBlock::Paragraph {
                spans: text_spans("kept"),
            },
        ],
        &map(&[]),
    );
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[0], Block::Heading1 { .. }));
    let j = json(&blocks);
    assert_eq!(j[1]["paragraph"]["rich_text"][0]["text"]["content"], "kept");
}

#[test]
fn notion_transformers_rewrite_converted_blocks() {
    // A typed step: promote every paragraph to a quote.
    let converter = Converter::new().with_notion_transformer(|blocks| {
        blocks
            .into_iter()
            .map(|b| match b {
                Block::Paragraph { paragraph } => Block::Quote {
                    quote: notionrs_types::object::block::quote::QuoteBlock::default()
                        .rich_text(paragraph.rich_text),
                },
                other => other,
            })
            .collect()
    });
    let blocks = converter.convert(
        &[OrgBlock::Paragraph {
            spans: text_spans("p"),
        }],
        &map(&[]),
    );
    assert!(matches!(blocks[0], Block::Quote { .. }));
    assert_eq!(
        json(&blocks)[0]["quote"]["rich_text"][0]["text"]["content"],
        "p"
    );
}

#[test]
fn org_transformer_output_feeds_notion_transformer() {
    let converter = Converter::new()
        .with_org_transformer(|blocks| {
            blocks
                .into_iter()
                .map(|b| match b {
                    OrgBlock::Paragraph { spans } => OrgBlock::Quote { spans },
                    other => other,
                })
                .collect()
        })
        .with_notion_transformer(|blocks| {
            blocks
                .into_iter()
                .filter(|b| matches!(b, Block::Quote { .. }))
                .collect()
        });
    let blocks = converter.convert(
        &[
            OrgBlock::Paragraph {
                spans: text_spans("promoted"),
            },
            OrgBlock::BulletItem {
                spans: text_spans("filtered out"),
            },
        ],
        &map(&[]),
    );
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        json(&blocks)[0]["quote"]["rich_text"][0]["text"]["content"],
        "promoted"
    );
}

#[test]
fn helper_shapes_match_notion_api() {
    let mention = serde_json::to_value(page_mention("pid")).unwrap();
    assert_eq!(mention["type"], "mention");
    assert_eq!(mention["mention"]["type"], "page");
    assert_eq!(mention["mention"]["page"]["id"], "pid");

    let run = serde_json::to_value(text_run("hi")).unwrap();
    assert_eq!(run["type"], "text");
    assert_eq!(run["text"]["content"], "hi");
    assert_eq!(run["annotations"]["bold"], false);
}

#[test]
fn rich_text_accessors_cover_every_inline_block_kind() {
    use notionrs_types::object::block::{
        bulleted_list_item::BulletedListItemBlock, callout::CalloutBlock, code::CodeBlock,
        heading::HeadingBlock, numbered_list_item::NumberedListItemBlock,
        paragraph::ParagraphBlock, quote::QuoteBlock, to_do::ToDoBlock, toggle::ToggleBlock,
    };
    use orgnotion::converter::{rich_text_mut, rich_text_of};

    let runs = vec![text_run("x")];
    let with_rich_text = [
        Block::Paragraph {
            paragraph: ParagraphBlock::default().rich_text(runs.clone()),
        },
        Block::Heading1 {
            heading_1: HeadingBlock::default().rich_text(runs.clone()),
        },
        Block::Heading2 {
            heading_2: HeadingBlock::default().rich_text(runs.clone()),
        },
        Block::Heading3 {
            heading_3: HeadingBlock::default().rich_text(runs.clone()),
        },
        Block::Heading4 {
            heading_4: HeadingBlock::default().rich_text(runs.clone()),
        },
        Block::BulletedListItem {
            bulleted_list_item: BulletedListItemBlock::default().rich_text(runs.clone()),
        },
        Block::NumberedListItem {
            numbered_list_item: NumberedListItemBlock::default().rich_text(runs.clone()),
        },
        Block::Quote {
            quote: QuoteBlock::default().rich_text(runs.clone()),
        },
        Block::Toggle {
            toggle: ToggleBlock::default().rich_text(runs.clone()),
        },
        Block::ToDo {
            to_do: ToDoBlock::default().rich_text(runs.clone()),
        },
        Block::Callout {
            callout: CalloutBlock::default().rich_text(runs.clone()),
        },
    ];
    for mut block in with_rich_text {
        assert_eq!(
            rich_text_of(&block).map(<[_]>::len),
            Some(1),
            "expected inline rich text on {block:?}"
        );
        rich_text_mut(&mut block)
            .expect("mutable access matches read access")
            .push(text_run("y"));
        assert_eq!(rich_text_of(&block).map(<[_]>::len), Some(2));
    }

    // Code is deliberately excluded (verbatim content, not prose), and
    // kinds without inline rich text yield None.
    let without = [
        Block::Code {
            code: CodeBlock {
                caption: Vec::new(),
                rich_text: runs,
                language: notionrs_types::object::language::Language::Rust,
            },
        },
        // zero_sized_map_values: HashMap<(), ()> is notionrs's own payload
        // type for Divider; we have no say in its shape.
        #[allow(clippy::zero_sized_map_values)]
        Block::Divider {
            divider: std::collections::HashMap::new(),
        },
    ];
    for mut block in without {
        assert!(rich_text_of(&block).is_none());
        assert!(rich_text_mut(&mut block).is_none());
    }
}
