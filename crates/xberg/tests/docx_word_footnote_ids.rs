//! Word numbers the separator lines in `footnotes.xml`/`endnotes.xml` -1 and 0 and
//! its first real note 1. The note parser skipped id 1 as a separator, so the first
//! footnote and the first endnote of every Word document disappeared, marker and
//! text. Separators are now identified by their `w:type`.

#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)] // ~keep: test/bench binaries print by design; org logging policy exempts tests
#![cfg(feature = "office")]

mod helpers;
use helpers::extract_bytes_document_blocking;

use xberg::core::config::ExtractionConfig;

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

#[test]
fn first_footnote_and_endnote_of_a_word_document_are_kept() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/office/word_footnote_ids.docx");
    let bytes = std::fs::read(&path).expect("Word footnote fixture must be present");
    let doc = extract_bytes_document_blocking(&bytes, DOCX_MIME, &ExtractionConfig::default())
        .expect("extraction must succeed");

    assert!(
        doc.content.contains("Body sentence[^1] continues[^2]"),
        "the first footnote's marker is missing from {:?}",
        doc.content
    );
    for note in ["FIRST FOOTNOTE", "SECOND FOOTNOTE", "FIRST ENDNOTE"] {
        assert!(doc.content.contains(note), "{note:?} missing from {:?}", doc.content);
    }
}
