//! Org-mode parsing for org-roam nodes.
//!
//! Extracts exactly what `orgnotion` needs from an org-roam node file:
//!
//! - the node's `:ID:` property (from the file-level property drawer)
//!   or a `#+ID:` keyword
//! - a title (`#+TITLE:`, falling back to the first heading, falling back
//!   to the filename)
//! - `[[id:UUID]]` / `[[id:UUID][description]]` links to other org-roam
//!   nodes; an org `::search-target` suffix after the UUID (e.g.
//!   `[[id:UUID::§1.2]]`) is dropped, the link resolves to the node
//! - file-level tags (`#+filetags:`)
//! - a coarse block structure (headings, paragraphs, list items, quotes,
//!   source blocks) good enough to convert into Notion blocks
//!
//! Backed by the [`orgize`] crate; unsupported constructs degrade to
//! plain paragraphs rather than failing the run.

use orgize::{
    Org, SyntaxElement, SyntaxKind, SyntaxNode,
    ast::{Document, Headline, Link, SourceBlock},
    rowan::{NodeOrToken, ast::AstNode},
};
use std::path::{Path, PathBuf};

/// A single org-roam node, parsed from one `.org` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The org-roam ID (from `:ID:` / `#+ID:`).
    pub id: String,
    /// Path of the source `.org` file.
    pub file_path: PathBuf,
    /// Display title of the node.
    pub title: String,
    /// Org-roam IDs of every node this node links to (deduplicated, in
    /// order of first appearance).
    pub links: Vec<String>,
    /// File-level tags (from `#+filetags:`), without the `:` delimiters.
    pub tags: Vec<String>,
    /// The node's body as coarse content blocks.
    pub blocks: Vec<OrgBlock>,
}

impl Node {
    /// Whether this node carries `tag` (case-insensitive).
    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }
}

/// A coarse content block, roughly one Notion block per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgBlock {
    /// `* Heading` (level = number of stars).
    Heading {
        /// Number of leading stars.
        level: u8,
        /// Inline content of the heading line.
        spans: Vec<Span>,
    },
    /// A run of contiguous plain-text lines.
    Paragraph {
        /// Inline content.
        spans: Vec<Span>,
    },
    /// `- item` / `+ item`.
    BulletItem {
        /// Inline content.
        spans: Vec<Span>,
    },
    /// `1. item` / `1) item`.
    NumberedItem {
        /// Inline content.
        spans: Vec<Span>,
    },
    /// `#+BEGIN_QUOTE` … `#+END_QUOTE`.
    Quote {
        /// Inline content of the quoted text.
        spans: Vec<Span>,
    },
    /// `#+BEGIN_SRC lang` … `#+END_SRC`.
    CodeBlock {
        /// Language tag after `#+BEGIN_SRC`, if any.
        language: Option<String>,
        /// Verbatim source text.
        content: String,
    },
}

/// A run of inline content: plain text, emphasized text, or a link to
/// another org-roam node (by ID).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    /// Literal text.
    Text(String),
    /// Emphasized text (`*bold*`, `/italic/`, …); the delimiter
    /// characters are not part of `text`.
    Marked {
        /// The emphasized text.
        text: String,
        /// Which org emphasis produced it.
        markup: Markup,
    },
    /// An `[[id:...]]` link to another node.
    LinkRef {
        /// Target node's org-roam ID.
        id: String,
        /// Link description, if the org link had one.
        description: Option<String>,
    },
}

/// Org inline emphasis kinds. `~code~` and `=verbatim=` both map to
/// [`Markup::Code`] — Notion has a single inline-code style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Markup {
    /// `*bold*`
    Bold,
    /// `/italic/`
    Italic,
    /// `_underline_`
    Underline,
    /// `+strike-through+`
    Strikethrough,
    /// `~code~` / `=verbatim=`
    Code,
}

/// Failure to parse one `.org` file into a node.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The file has no `:ID:` property and no `#+ID:` keyword.
    #[error("{}: missing :ID: property (or #+ID: keyword) in file preamble", .path.display())]
    MissingId {
        /// The file that failed.
        path: PathBuf,
    },
}

impl OrgBlock {
    /// The inline spans of this block (empty for code blocks).
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        match self {
            OrgBlock::Heading { spans, .. }
            | OrgBlock::Paragraph { spans }
            | OrgBlock::BulletItem { spans }
            | OrgBlock::NumberedItem { spans }
            | OrgBlock::Quote { spans } => spans,
            OrgBlock::CodeBlock { .. } => &[],
        }
    }
}

/// Parse a single org-roam node file's raw text.
///
/// `file_path` is used for error messages and stored on the returned
/// [`Node`].
///
/// # Errors
///
/// Fails if no `:ID:` property (or `#+ID:` keyword) is found in the
/// file-level preamble.
pub fn parse_node(file_path: &Path, text: &str) -> Result<Node, ParseError> {
    let org = Org::parse(text);
    let doc = org.document();

    let id = extract_id(&doc).ok_or_else(|| ParseError::MissingId {
        path: file_path.to_path_buf(),
    })?;

    let mut blocks = Vec::new();
    if let Some(section) = doc.section() {
        walk_section(section.syntax(), &mut blocks);
    }
    for headline in doc.headlines() {
        walk_headline(&headline, &mut blocks);
    }

    let title = doc
        .title()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| {
            blocks.iter().find_map(|b| match b {
                OrgBlock::Heading { spans, .. } => Some(spans_to_plain_text(spans)),
                _ => None,
            })
        })
        .unwrap_or_else(|| {
            file_path.file_stem().map_or_else(
                || "untitled".to_string(),
                |s| s.to_string_lossy().to_string(),
            )
        });

    let mut link_ids: Vec<String> = Vec::new();
    for block in &blocks {
        for span in block.spans() {
            if let Span::LinkRef { id, .. } = span
                && !link_ids.contains(id)
            {
                link_ids.push(id.clone());
            }
        }
    }

    Ok(Node {
        id,
        file_path: file_path.to_path_buf(),
        title,
        links: link_ids,
        tags: extract_filetags(&doc),
        blocks,
    })
}

/// Tags from the `#+filetags:` keyword. Handles the org-roam v2 form
/// (`:a:b:`) as well as legacy space-separated values.
fn extract_filetags(doc: &Document) -> Vec<String> {
    doc.keywords()
        .filter(|k| k.key().eq_ignore_ascii_case("FILETAGS"))
        .flat_map(|k| {
            k.value()
                .split([':', ' ', '\t'])
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn extract_id(doc: &Document) -> Option<String> {
    if let Some(drawer) = doc.properties()
        && let Some((_, value)) = drawer
            .iter()
            .find(|(k, _)| k.as_ref().eq_ignore_ascii_case("ID"))
    {
        return Some(value.as_ref().trim().to_string());
    }
    doc.keywords()
        .find(|k| k.key().eq_ignore_ascii_case("ID"))
        .map(|k| k.value().as_ref().trim().to_string())
}

fn walk_section(section: &SyntaxNode, blocks: &mut Vec<OrgBlock>) {
    for child in section.children() {
        match child.kind() {
            SyntaxKind::PARAGRAPH => {
                let spans = spans_of_node(&child);
                if !is_effectively_empty(&spans) {
                    blocks.push(OrgBlock::Paragraph { spans });
                }
            }
            SyntaxKind::LIST => walk_list(&child, blocks),
            SyntaxKind::QUOTE_BLOCK => {
                let spans = child
                    .children()
                    .find(|n| n.kind() == SyntaxKind::BLOCK_CONTENT)
                    .map_or_else(
                        || vec![Span::Text(String::new())],
                        |content| spans_of_node(&content),
                    );
                blocks.push(OrgBlock::Quote { spans });
            }
            SyntaxKind::SOURCE_BLOCK => {
                let block = SourceBlock::cast(child.clone()).expect("SOURCE_BLOCK casts");
                let language = block.language().map(|t| t.as_ref().to_string());
                let content = block.value().trim_end_matches('\n').to_string();
                blocks.push(OrgBlock::CodeBlock { language, content });
            }
            _ => {}
        }
    }
}

fn walk_list(list: &SyntaxNode, blocks: &mut Vec<OrgBlock>) {
    for item in list
        .children()
        .filter(|n| n.kind() == SyntaxKind::LIST_ITEM)
    {
        let is_ordered = item
            .children_with_tokens()
            .find_map(|e| match e {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::LIST_ITEM_BULLET => Some(
                    t.text()
                        .trim_start()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit()),
                ),
                _ => None,
            })
            .unwrap_or(false);

        if let Some(content) = item
            .children()
            .find(|n| n.kind() == SyntaxKind::LIST_ITEM_CONTENT)
        {
            if let Some(paragraph) = content
                .children()
                .find(|n| n.kind() == SyntaxKind::PARAGRAPH)
            {
                let spans = spans_of_node(&paragraph);
                if is_ordered {
                    blocks.push(OrgBlock::NumberedItem { spans });
                } else {
                    blocks.push(OrgBlock::BulletItem { spans });
                }
            }
            for nested in content.children() {
                if nested.kind() == SyntaxKind::LIST {
                    walk_list(&nested, blocks);
                }
            }
        }
    }
}

fn walk_headline(hl: &Headline, blocks: &mut Vec<OrgBlock>) {
    let level = u8::try_from(hl.level()).unwrap_or(u8::MAX);
    let spans = spans_of_elements(hl.title());
    blocks.push(OrgBlock::Heading { level, spans });
    if let Some(section) = hl.section() {
        walk_section(section.syntax(), blocks);
    }
    for child in hl.headlines() {
        walk_headline(&child, blocks);
    }
}

fn spans_of_node(node: &SyntaxNode) -> Vec<Span> {
    spans_of_elements(node.children_with_tokens())
}

/// Accumulates inline content into [`Span`]s, merging adjacent text of
/// the same markup into one span and flushing whenever the markup
/// changes (or a link interrupts the text).
#[derive(Default)]
struct SpanBuilder {
    spans: Vec<Span>,
    buf: String,
    buf_markup: Option<Markup>,
}

impl SpanBuilder {
    fn push_str(&mut self, text: &str, markup: Option<Markup>) {
        if markup != self.buf_markup {
            self.flush();
            self.buf_markup = markup;
        }
        self.buf.push_str(text);
    }

    fn push_span(&mut self, span: Span) {
        self.flush();
        self.spans.push(span);
    }

    fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.buf).replace('\n', " ");
        self.spans.push(match self.buf_markup {
            None => Span::Text(text),
            Some(markup) => Span::Marked { text, markup },
        });
    }

    fn finish(mut self) -> Vec<Span> {
        self.flush();
        let last_text = self.spans.last_mut().and_then(|s| match s {
            Span::Text(t) | Span::Marked { text: t, .. } => Some(t),
            Span::LinkRef { .. } => None,
        });
        if let Some(t) = last_text {
            let trimmed = t.trim_end().to_string();
            if trimmed.is_empty() {
                self.spans.pop();
            } else {
                *t = trimmed;
            }
        }
        if self.spans.is_empty() {
            self.spans.push(Span::Text(String::new()));
        }
        self.spans
    }
}

fn spans_of_elements(elements: impl Iterator<Item = SyntaxElement>) -> Vec<Span> {
    let mut builder = SpanBuilder::default();
    for el in elements {
        visit_element(el, &mut builder, None);
    }
    builder.finish()
}

fn visit_element(el: SyntaxElement, builder: &mut SpanBuilder, markup: Option<Markup>) {
    match el {
        NodeOrToken::Token(t) => match t.kind() {
            SyntaxKind::BLANK_LINE => {}
            _ => builder.push_str(t.text(), markup),
        },
        NodeOrToken::Node(n) => {
            if n.kind() == SyntaxKind::LINK {
                visit_link(&n, builder, markup);
                return;
            }
            if let Some((inner_markup, delimiter)) = markup_of(n.kind()) {
                // Recurse with the emphasis style, dropping the delimiter
                // tokens — inner content can't contain them (a matching
                // delimiter would have terminated the emphasis).
                for child in n.children_with_tokens() {
                    let is_delimiter =
                        matches!(&child, NodeOrToken::Token(t) if t.kind() == delimiter);
                    if !is_delimiter {
                        visit_element(child, builder, Some(inner_markup));
                    }
                }
                return;
            }
            for child in n.children_with_tokens() {
                visit_element(child, builder, markup);
            }
        }
    }
}

fn visit_link(n: &SyntaxNode, builder: &mut SpanBuilder, markup: Option<Markup>) {
    let Some(link) = Link::cast(n.clone()) else {
        return;
    };
    let path = link.path().as_ref().to_string();
    if let Some(id) = path.strip_prefix("id:") {
        // Org allows a `::search-target` suffix after the link path; the
        // org-roam node id is only the part before it.
        let id = id.split_once("::").map_or(id, |(id, _)| id);
        let description = if link.has_description() {
            Some(link.description_raw())
        } else {
            None
        };
        builder.push_span(Span::LinkRef {
            id: id.to_string(),
            description,
        });
    } else if link.has_description() {
        builder.push_str(&link.description_raw(), markup);
    } else {
        builder.push_str(&path, markup);
    }
}

/// The [`Markup`] and delimiter token kind of an org emphasis node.
fn markup_of(kind: SyntaxKind) -> Option<(Markup, SyntaxKind)> {
    match kind {
        SyntaxKind::BOLD => Some((Markup::Bold, SyntaxKind::STAR)),
        SyntaxKind::ITALIC => Some((Markup::Italic, SyntaxKind::SLASH)),
        SyntaxKind::UNDERLINE => Some((Markup::Underline, SyntaxKind::UNDERSCORE)),
        SyntaxKind::STRIKE => Some((Markup::Strikethrough, SyntaxKind::PLUS)),
        SyntaxKind::CODE => Some((Markup::Code, SyntaxKind::TILDE)),
        SyntaxKind::VERBATIM => Some((Markup::Code, SyntaxKind::EQUAL)),
        _ => None,
    }
}

fn is_effectively_empty(spans: &[Span]) -> bool {
    spans.iter().all(|s| match s {
        Span::Text(t) | Span::Marked { text: t, .. } => t.trim().is_empty(),
        Span::LinkRef { .. } => false,
    })
}

/// Flatten spans to plain text (link descriptions or IDs stand in for
/// links) — used for the title fallback.
fn spans_to_plain_text(spans: &[Span]) -> String {
    spans
        .iter()
        .map(|s| match s {
            Span::Text(t) | Span::Marked { text: t, .. } => t.clone(),
            Span::LinkRef { description, id } => description.clone().unwrap_or_else(|| id.clone()),
        })
        .collect::<String>()
}
