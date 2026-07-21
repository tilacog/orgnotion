//! Converts [`OrgBlock`]s into typed Notion blocks
//! ([`notionrs_types::object::block::Block`]).
//!
//! The conversion is an extensible pipeline ([`Converter`]): transformer
//! functions may rewrite the org blocks before conversion and the typed
//! Notion blocks after it, on top of the built-in block mapping.
//!
//! Org-roam links ([`Span::LinkRef`]) are rewritten into Notion page
//! *mentions* using the supplied org-ID → Notion-page-ID map, rather than
//! raw URLs — mentions carry the target page ID explicitly in the API
//! response, which is what makes post-validation exact.

use crate::org_parser::{Markup, OrgBlock, Span};
use notionrs_types::object::block::{
    Block, bulleted_list_item::BulletedListItemBlock, code::CodeBlock, heading::HeadingBlock,
    numbered_list_item::NumberedListItemBlock, paragraph::ParagraphBlock, quote::QuoteBlock,
};
use notionrs_types::object::language::Language;
use notionrs_types::object::rich_text::{
    RichText, RichTextAnnotations,
    mention::{Mention, PageMention},
    text::{Text, TextLink},
};
use std::collections::HashMap;
use std::str::FromStr;

/// How an `[[id:...]]` link resolves in the published snapshot.
#[derive(Debug, Clone)]
pub enum LinkTarget {
    /// A page mention — the link target has its own Notion page.
    Page(String),
    /// A URL link to a specific block on a page — the link target is
    /// merged onto a directory page, and `url` points at its heading
    /// block (`https://notion.so/{page_id}#{block_id}`).
    Block {
        /// The Notion page ID the block lives on.
        page_id: String,
        /// The full URL with anchor (`page_url#block_id`).
        url: String,
        /// Display text for the link.
        text: String,
    },
}

/// Notion's rich-text content length limit per text object.
const MAX_TEXT_LEN: usize = 2000;

/// Notion's hard limit on the length of a `rich_text` array in one block.
const MAX_RICH_TEXT_ITEMS: usize = 100;

/// A pipeline step over the parsed org blocks, applied before the core
/// conversion. Steps may rewrite, drop, merge, or insert blocks.
pub type OrgTransformer = Box<dyn Fn(Vec<OrgBlock>) -> Vec<OrgBlock>>;

/// A pipeline step over the produced typed Notion blocks, applied after
/// the core conversion.
pub type NotionTransformer = Box<dyn Fn(Vec<Block>) -> Vec<Block>>;

/// Org → Notion block converter with an extensible transformation
/// pipeline.
///
/// Conversion runs in three stages: every registered [`OrgTransformer`]
/// in registration order, then the built-in block conversion, then every
/// registered [`NotionTransformer`] in registration order. A `Converter`
/// with no transformers behaves exactly like [`convert_blocks`].
#[derive(Default)]
pub struct Converter {
    org_transformers: Vec<OrgTransformer>,
    notion_transformers: Vec<NotionTransformer>,
}

impl Converter {
    /// A converter with no extra pipeline steps.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a step that rewrites the org blocks before conversion.
    #[must_use]
    pub fn with_org_transformer(
        mut self,
        step: impl Fn(Vec<OrgBlock>) -> Vec<OrgBlock> + 'static,
    ) -> Self {
        self.org_transformers.push(Box::new(step));
        self
    }

    /// Append a step that rewrites the typed Notion blocks after
    /// conversion.
    #[must_use]
    pub fn with_notion_transformer(
        mut self,
        step: impl Fn(Vec<Block>) -> Vec<Block> + 'static,
    ) -> Self {
        self.notion_transformers.push(Box::new(step));
        self
    }

    /// Run the full pipeline over a node's blocks.
    ///
    /// `id_to_link` maps org-roam node ID → [`LinkTarget`] (page mention
    /// or block URL), and must already contain an entry for every node
    /// in the vault (all node pages are created blank before any content
    /// is written, precisely so this map is complete before conversion
    /// starts).
    // implicit_hasher: this is an application, not a library API — callers
    // always pass the std default-hashed map built by the run orchestration.
    #[allow(clippy::implicit_hasher)]
    #[must_use]
    pub fn convert(
        &self,
        blocks: &[OrgBlock],
        id_to_link: &HashMap<String, LinkTarget>,
    ) -> Vec<Block> {
        let blocks = self
            .org_transformers
            .iter()
            .fold(blocks.to_vec(), |acc, step| step(acc));
        let converted = blocks
            .iter()
            .map(|b| convert_block(b, id_to_link))
            .collect();
        self.notion_transformers
            .iter()
            .fold(converted, |acc, step| step(acc))
    }
}

/// Convert every block of a node into typed Notion blocks with no extra
/// pipeline steps. Shorthand for [`Converter::new().convert(...)`];
/// see [`Converter::convert`] for the `id_to_link` contract.
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn convert_blocks(
    blocks: &[OrgBlock],
    id_to_link: &HashMap<String, LinkTarget>,
) -> Vec<Block> {
    Converter::new().convert(blocks, id_to_link)
}

fn convert_block(block: &OrgBlock, id_to_link: &HashMap<String, LinkTarget>) -> Block {
    match block {
        OrgBlock::Heading { level, spans } => {
            let heading = HeadingBlock::default().rich_text(rich_text(spans, id_to_link));
            match level {
                1 => Block::Heading1 { heading_1: heading },
                2 => Block::Heading2 { heading_2: heading },
                _ => Block::Heading3 { heading_3: heading },
            }
        }
        OrgBlock::Paragraph { spans } => Block::Paragraph {
            paragraph: ParagraphBlock::default().rich_text(rich_text(spans, id_to_link)),
        },
        OrgBlock::BulletItem { spans } => Block::BulletedListItem {
            bulleted_list_item: BulletedListItemBlock::default()
                .rich_text(rich_text(spans, id_to_link)),
        },
        OrgBlock::NumberedItem { spans } => Block::NumberedListItem {
            numbered_list_item: NumberedListItemBlock::default()
                .rich_text(rich_text(spans, id_to_link)),
        },
        OrgBlock::Quote { spans } => Block::Quote {
            quote: QuoteBlock::default().rich_text(rich_text(spans, id_to_link)),
        },
        OrgBlock::CodeBlock { language, content } => Block::Code {
            code: CodeBlock {
                caption: Vec::new(),
                rich_text: chunked_text_runs(content),
                language: normalize_code_language(language.as_deref()),
            },
        },
    }
}

/// Build a rich-text `mention` pointing at a Notion page — this is how
/// `[[id:...]]` org-roam links are represented in Notion.
#[must_use]
pub fn page_mention(page_id: &str) -> RichText {
    RichText::Mention {
        mention: Mention::Page {
            page: PageMention::from(page_id),
        },
        annotations: RichTextAnnotations::default(),
        plain_text: String::new(),
        href: None,
    }
}

/// Build a plain rich-text run.
#[must_use]
pub fn text_run(content: &str) -> RichText {
    RichText::Text {
        text: Text {
            content: content.to_string(),
            link: None,
        },
        annotations: RichTextAnnotations::default(),
        plain_text: content.to_string(),
        href: None,
    }
}

/// The inline rich text of a block, for the block kinds this tool reads
/// or rewrites. `None` for code blocks (their text is verbatim content,
/// not prose) and for block kinds without inline rich text.
#[must_use]
pub fn rich_text_of(block: &Block) -> Option<&[RichText]> {
    match block {
        Block::Paragraph { paragraph } => Some(&paragraph.rich_text),
        Block::Heading1 { heading_1 } => Some(&heading_1.rich_text),
        Block::Heading2 { heading_2 } => Some(&heading_2.rich_text),
        Block::Heading3 { heading_3 } => Some(&heading_3.rich_text),
        Block::Heading4 { heading_4 } => Some(&heading_4.rich_text),
        Block::BulletedListItem { bulleted_list_item } => Some(&bulleted_list_item.rich_text),
        Block::NumberedListItem { numbered_list_item } => Some(&numbered_list_item.rich_text),
        Block::Quote { quote } => Some(&quote.rich_text),
        Block::Toggle { toggle } => Some(&toggle.rich_text),
        Block::ToDo { to_do } => Some(&to_do.rich_text),
        Block::Callout { callout } => Some(&callout.rich_text),
        _ => None,
    }
}

/// Mutable variant of [`rich_text_of`], for pipeline steps that rewrite
/// runs in place.
#[must_use]
pub fn rich_text_mut(block: &mut Block) -> Option<&mut Vec<RichText>> {
    match block {
        Block::Paragraph { paragraph } => Some(&mut paragraph.rich_text),
        Block::Heading1 { heading_1 } => Some(&mut heading_1.rich_text),
        Block::Heading2 { heading_2 } => Some(&mut heading_2.rich_text),
        Block::Heading3 { heading_3 } => Some(&mut heading_3.rich_text),
        Block::Heading4 { heading_4 } => Some(&mut heading_4.rich_text),
        Block::BulletedListItem { bulleted_list_item } => Some(&mut bulleted_list_item.rich_text),
        Block::NumberedListItem { numbered_list_item } => Some(&mut numbered_list_item.rich_text),
        Block::Quote { quote } => Some(&mut quote.rich_text),
        Block::Toggle { toggle } => Some(&mut toggle.rich_text),
        Block::ToDo { to_do } => Some(&mut to_do.rich_text),
        Block::Callout { callout } => Some(&mut callout.rich_text),
        _ => None,
    }
}

/// Turn a run of [`Span`]s into Notion rich text. An unresolvable link
/// (a node ID not present in `id_to_link`) degrades to plain text instead
/// of crashing the run.
///
/// Pre-validation should already guarantee every link resolves within the
/// vault, so the fallback is defensive only.
fn rich_text(spans: &[Span], id_to_link: &HashMap<String, LinkTarget>) -> Vec<RichText> {
    let mut out = Vec::new();
    for span in spans {
        match span {
            Span::Text(t) => out.extend(chunked_text_runs(t)),
            Span::Marked { text, markup } => {
                out.extend(chunked_annotated_runs(text, annotations_for(*markup)));
            }
            Span::LinkRef { id, description } => {
                match id_to_link.get(id) {
                    Some(LinkTarget::Page(page_id)) => {
                        out.push(page_mention(page_id));
                    }
                    Some(LinkTarget::Block { url, text, .. }) => {
                        out.extend(chunked_link_runs(text, url));
                    }
                    None => {
                        let fallback = description
                            .clone()
                            .unwrap_or_else(|| format!("[[id:{id}]]"));
                        out.extend(chunked_text_runs(&fallback));
                    }
                }
            }
            Span::ExternalLink { url, description } => {
                let text = description.as_deref().unwrap_or(url);
                out.extend(chunked_link_runs(text, url));
            }
        }
    }
    if out.is_empty() {
        out.push(text_run(""));
    }
    out.truncate(MAX_RICH_TEXT_ITEMS);
    out
}

/// Split text into rich-text runs no longer than Notion's per-object
/// content limit.
fn chunked_text_runs(text: &str) -> Vec<RichText> {
    chunked_annotated_runs(text, RichTextAnnotations::default())
}

/// Like [`chunked_text_runs`], with the same annotations on every run.
fn chunked_annotated_runs(text: &str, annotations: RichTextAnnotations) -> Vec<RichText> {
    let annotated = |content: &str| {
        let mut run = text_run(content);
        if let RichText::Text { annotations: a, .. } = &mut run {
            *a = annotations;
        }
        run
    };
    if text.is_empty() {
        return vec![annotated("")];
    }
    text.chars()
        .collect::<Vec<char>>()
        .chunks(MAX_TEXT_LEN)
        .map(|c| annotated(&c.iter().collect::<String>()))
        .collect()
}

/// Like [`chunked_text_runs`] but every run carries `url` as an inline
/// link — how block-level links to merged sections are represented.
fn chunked_link_runs(text: &str, url: &str) -> Vec<RichText> {
    let linked = |content: &str| {
        RichText::Text {
            text: Text {
                content: content.to_string(),
                link: Some(TextLink { url: url.to_string() }),
            },
            annotations: RichTextAnnotations::default(),
            plain_text: content.to_string(),
            href: Some(url.to_string()),
        }
    };
    if text.is_empty() {
        return vec![linked("")];
    }
    text.chars()
        .collect::<Vec<char>>()
        .chunks(MAX_TEXT_LEN)
        .map(|c| linked(&c.iter().collect::<String>()))
        .collect()
}

/// The Notion annotation set for one org emphasis kind.
fn annotations_for(markup: Markup) -> RichTextAnnotations {
    let mut a = RichTextAnnotations::default();
    match markup {
        Markup::Bold => a.bold = true,
        Markup::Italic => a.italic = true,
        Markup::Underline => a.underline = true,
        Markup::Strikethrough => a.strikethrough = true,
        Markup::Code => a.code = true,
    }
    a
}

/// Map an org source-block language tag onto Notion's [`Language`] enum;
/// anything Notion doesn't know degrades to plain text rather than
/// failing the whole request.
fn normalize_code_language(language: Option<&str>) -> Language {
    let Some(lang) = language else {
        return Language::PlainText;
    };
    let lower = lang.to_lowercase();
    let alias = match lower.as_str() {
        "sh" | "zsh" => "shell",
        "js" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "rs" => "rust",
        "elisp" | "emacs-lisp" => "lisp",
        other => other,
    };
    Language::from_str(alias).unwrap_or(Language::PlainText)
}
