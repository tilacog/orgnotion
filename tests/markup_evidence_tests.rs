//! Evidence test for the org-markup bug: emphasis delimiters (`*`, `~`,
//! `/`, …) must not leak into span text as literal characters.

use orgnotion::org_parser::{Span, parse_node};
use std::path::PathBuf;

#[test]
fn markup_delimiters_never_appear_in_span_text() {
    let node = parse_node(
        &PathBuf::from("test.org"),
        ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: T\n\n\
         *SOL wrapping at driver vs solver.* Mirrors EVM ~ManageNativeToken~ but tangles.\n",
    )
    .unwrap();
    for block in &node.blocks {
        for span in block.spans() {
            if let Span::Text(t) = span {
                assert!(
                    !t.contains('*') && !t.contains('~'),
                    "markup delimiters leaked into plain text: {t:?}"
                );
            }
        }
    }
}
