//! Optional pipeline steps for the [`crate::converter::Converter`].
//!
//! Kept separate from the core conversion so steps can evolve (or be
//! swapped out) without touching the block mapping itself.

use crate::converter::{rich_text_mut, text_run};
use notionrs_types::object::block::{Block, callout::CalloutBlock};
use notionrs_types::object::color::Color;
use notionrs_types::object::emoji::Emoji;
use notionrs_types::object::emoji_and_icon::EmojiAndIcon;
use notionrs_types::object::rich_text::{RichText, RichTextAnnotations, text::Text};

/// Notion's hard limit on the length of a `rich_text` array in one block.
const MAX_RICH_TEXT_ITEMS: usize = 100;

/// The warning shown on pages built from `:unreviewed:` nodes.
const UNREVIEWED_WARNING: &str = "This content hasn't been reviewed yet, proceed with caution";

/// A [`crate::converter::NotionTransformer`] step that renders org
/// dedicated targets (`<<§1.2.3>>`) as bold text.
///
/// The parser passes targets through verbatim inside text spans, so after
/// conversion they sit in plain rich-text runs as literal `<<…>>`. This
/// step splits those runs and re-emits the target text (without the angle
/// brackets) as a bold-annotated run. Code blocks are left verbatim
/// ([`rich_text_mut`] excludes them), as are mention runs and runs that
/// already carry annotations.
#[must_use]
pub fn anchors_to_bold(blocks: Vec<Block>) -> Vec<Block> {
    blocks
        .into_iter()
        .map(|mut block| {
            if let Some(rich_text) = rich_text_mut(&mut block) {
                let mut rewritten: Vec<RichText> =
                    rich_text.drain(..).flat_map(rewrite_run).collect();
                rewritten.truncate(MAX_RICH_TEXT_ITEMS);
                *rich_text = rewritten;
            }
            block
        })
        .collect()
}

/// A [`crate::converter::NotionTransformer`] step that flags a page built
/// from an `:unreviewed:` org-roam node: the first level-1 heading is
/// colored red and a red-text ⚠️ warning callout is inserted right below
/// it. A page with no level-1 heading gets the callout as its first block.
///
/// Applied per node (after conversion) rather than registered on the
/// shared [`crate::converter::Converter`], since only tagged nodes get it.
#[must_use]
pub fn unreviewed_banner(mut blocks: Vec<Block>) -> Vec<Block> {
    let first_h1 = blocks
        .iter_mut()
        .enumerate()
        .find_map(|(i, block)| match block {
            Block::Heading1 { heading_1 } => Some((i, heading_1)),
            _ => None,
        });
    let callout_at = match first_h1 {
        Some((i, heading)) => {
            for run in &mut heading.rich_text {
                set_color(run, Color::Red);
            }
            i + 1
        }
        None => 0,
    };
    blocks.insert(callout_at, warning_callout());
    blocks
}

fn warning_callout() -> Block {
    let mut text = text_run(UNREVIEWED_WARNING);
    set_color(&mut text, Color::Red);
    Block::Callout {
        callout: CalloutBlock::default()
            .rich_text(vec![text])
            .icon(EmojiAndIcon::Emoji(Emoji::from("⚠️"))),
    }
}

fn set_color(run: &mut RichText, color: Color) {
    let (RichText::Text { annotations, .. }
    | RichText::Mention { annotations, .. }
    | RichText::Equation { annotations, .. }) = run;
    annotations.color = color;
}

/// Rewrite one rich-text run, splitting out `<<…>>` targets as bold runs.
/// Anything that is not a plain unannotated, unlinked text run passes
/// through.
fn rewrite_run(run: RichText) -> Vec<RichText> {
    match run {
        RichText::Text {
            ref text,
            annotations,
            ..
        } if annotations == RichTextAnnotations::default() && text.link.is_none() => {
            split_anchors(&text.content)
                .into_iter()
                .map(|segment| match segment {
                    Segment::Plain(t) => text_run(t),
                    Segment::Anchor(t) => bold_run(t),
                })
                .collect()
        }
        other => vec![other],
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

enum Segment<'a> {
    Plain(&'a str),
    Anchor(&'a str),
}

/// Split text into plain segments and `<<…>>` anchor segments (yielding
/// the anchor's inner text). A `<<` without a well-formed target — empty,
/// containing `<`, `>`, or a newline, or unterminated — stays literal.
fn split_anchors(text: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut plain_start = 0;
    let mut cursor = 0;
    while let Some(open) = text[cursor..].find("<<") {
        let open = cursor + open;
        let inner_start = open + 2;
        let Some(close) = text[inner_start..].find(">>") else {
            break;
        };
        let inner = &text[inner_start..inner_start + close];
        if inner.is_empty() || inner.contains(['<', '>', '\n']) {
            cursor = open + 1;
            continue;
        }
        if plain_start < open {
            segments.push(Segment::Plain(&text[plain_start..open]));
        }
        segments.push(Segment::Anchor(inner));
        plain_start = inner_start + close + 2;
        cursor = plain_start;
    }
    if plain_start < text.len() || segments.is_empty() {
        segments.push(Segment::Plain(&text[plain_start..]));
    }
    segments
}
