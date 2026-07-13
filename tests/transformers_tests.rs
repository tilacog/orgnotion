//! Unit tests for the optional converter pipeline steps.

use notionrs_types::object::block::{
    Block, code::CodeBlock, heading::HeadingBlock, paragraph::ParagraphBlock,
};
use notionrs_types::object::color::Color;
use notionrs_types::object::emoji_and_icon::EmojiAndIcon;
use notionrs_types::object::language::Language;
use notionrs_types::object::rich_text::{RichText, RichTextAnnotations, text::Text};
use orgnotion::converter::{Converter, page_mention, text_run};
use orgnotion::org_parser::{OrgBlock, Span};
use orgnotion::transformers::{anchors_to_bold, unreviewed_banner};
use std::collections::HashMap;

fn paragraph(rich_text: Vec<RichText>) -> Block {
    Block::Paragraph {
        paragraph: ParagraphBlock::default().rich_text(rich_text),
    }
}

fn bold_run(content: &str) -> RichText {
    RichText::Text {
        text: Text {
            content: content.to_string(),
            link: None,
        },
        annotations: RichTextAnnotations {
            bold: true,
            ..RichTextAnnotations::default()
        },
        plain_text: content.to_string(),
        href: None,
    }
}

fn rich_text_of(block: &Block) -> &[RichText] {
    orgnotion::converter::rich_text_of(block).expect("block carries rich text")
}

#[test]
fn anchor_becomes_bold_run_between_plain_runs() {
    let blocks = vec![paragraph(vec![text_run("see <<§1.2.3>> for details")])];
    let out = anchors_to_bold(blocks);
    assert_eq!(
        rich_text_of(&out[0]),
        &[
            text_run("see "),
            bold_run("§1.2.3"),
            text_run(" for details")
        ]
    );
}

#[test]
fn multiple_anchors_in_one_run_all_become_bold() {
    let blocks = vec![paragraph(vec![text_run("<<a>> mid <<b>>")])];
    let out = anchors_to_bold(blocks);
    assert_eq!(
        rich_text_of(&out[0]),
        &[bold_run("a"), text_run(" mid "), bold_run("b")]
    );
}

#[test]
fn text_without_anchors_is_untouched() {
    let blocks = vec![paragraph(vec![text_run("plain, << unterminated")])];
    let out = anchors_to_bold(blocks);
    assert_eq!(rich_text_of(&out[0]), &[text_run("plain, << unterminated")]);
}

#[test]
fn malformed_targets_stay_literal() {
    let literal = "<<>> and <<a<b>> and <<x\ny>>";
    let blocks = vec![paragraph(vec![text_run(literal)])];
    let out = anchors_to_bold(blocks);
    assert_eq!(rich_text_of(&out[0]), &[text_run(literal)]);
}

#[test]
fn mentions_and_annotated_runs_pass_through() {
    let mention = page_mention("p1");
    let already_bold = bold_run("keep <<this>> literal");
    let blocks = vec![paragraph(vec![mention.clone(), already_bold.clone()])];
    let out = anchors_to_bold(blocks);
    assert_eq!(rich_text_of(&out[0]), &[mention, already_bold]);
}

#[test]
fn code_blocks_are_left_verbatim() {
    let code = Block::Code {
        code: CodeBlock {
            caption: vec![],
            rich_text: vec![text_run("let x = <<§1.2>>;")],
            language: Language::Rust,
        },
    };
    let out = anchors_to_bold(vec![code]);
    match &out[0] {
        Block::Code { code } => {
            assert_eq!(code.rich_text, vec![text_run("let x = <<§1.2>>;")]);
        }
        other => panic!("expected code block, got {other:?}"),
    }
}

fn h1(rich_text: Vec<RichText>) -> Block {
    Block::Heading1 {
        heading_1: HeadingBlock::default().rich_text(rich_text),
    }
}

fn run_color(run: &RichText) -> Color {
    match run {
        RichText::Text { annotations, .. }
        | RichText::Mention { annotations, .. }
        | RichText::Equation { annotations, .. } => annotations.color,
    }
}

fn assert_warning_callout(block: &Block) {
    let Block::Callout { callout } = block else {
        panic!("expected a callout, got {block:?}");
    };
    match &callout.rich_text[..] {
        [run @ RichText::Text { plain_text, .. }] => {
            assert_eq!(
                plain_text,
                "This content hasn't been reviewed yet, proceed with caution"
            );
            assert_eq!(run_color(run), Color::Red);
        }
        other => panic!("expected a single text run, got {other:?}"),
    }
    match callout.icon.as_ref().expect("callout carries an icon") {
        EmojiAndIcon::Emoji(emoji) => assert_eq!(emoji.emoji, "⚠️"),
        other => panic!("expected an emoji icon, got {other:?}"),
    }
}

#[test]
fn unreviewed_banner_reddens_first_h1_and_inserts_callout_below_it() {
    let blocks = vec![
        paragraph(vec![text_run("intro")]),
        h1(vec![text_run("Title "), page_mention("p1")]),
        h1(vec![text_run("Second")]),
    ];
    let out = unreviewed_banner(blocks);
    assert_eq!(out.len(), 4);
    for run in rich_text_of(&out[1]) {
        assert_eq!(run_color(run), Color::Red);
    }
    assert_warning_callout(&out[2]);
    // The paragraph and the second h1 are untouched.
    assert_eq!(rich_text_of(&out[0]), &[text_run("intro")]);
    assert_eq!(rich_text_of(&out[3]), &[text_run("Second")]);
}

#[test]
fn unreviewed_banner_without_h1_prepends_callout() {
    let blocks = vec![paragraph(vec![text_run("body")])];
    let out = unreviewed_banner(blocks);
    assert_eq!(out.len(), 2);
    assert_warning_callout(&out[0]);
    assert_eq!(rich_text_of(&out[1]), &[text_run("body")]);
}

#[test]
fn unreviewed_banner_on_empty_page_is_just_the_callout() {
    let out = unreviewed_banner(vec![]);
    assert_eq!(out.len(), 1);
    assert_warning_callout(&out[0]);
}

#[test]
fn works_end_to_end_through_the_converter_pipeline() {
    let org = vec![OrgBlock::Paragraph {
        spans: vec![Span::Text("anchor <<§4.5>> and a link ".to_string())],
    }];
    let converter = Converter::new().with_notion_transformer(anchors_to_bold);
    let out = converter.convert(&org, &HashMap::new());
    assert_eq!(
        rich_text_of(&out[0]),
        &[
            text_run("anchor "),
            bold_run("§4.5"),
            text_run(" and a link ")
        ]
    );
}
