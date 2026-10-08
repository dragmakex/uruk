//! The canonical report template and its deterministic PDF twin.
//!
//! REPORT.md is the canonical, model-free export built from persisted
//! records; REPORT.pdf is a byte-deterministic rendering of that exact
//! markdown. Both follow one fixed template contract: a version number and
//! a fixed section order that tools (and humans) can rely on across runs.

mod common;

use uruk::provider::MockProvider;
use uruk::records::{Mode, RunState};
use uruk::report::{self, REPORT_SECTIONS, REPORT_TEMPLATE_VERSION, render_report_pdf};

/// Section headings must appear in `text` in template order, each exactly
/// once.
fn assert_sections_in_order(text: &str) {
    let mut from = 0usize;
    for heading in REPORT_SECTIONS {
        let at = text[from..]
            .find(heading)
            .unwrap_or_else(|| panic!("section {heading:?} missing or out of order in the report"));
        from += at + heading.len();
        assert!(
            !text[from..].contains(heading),
            "section {heading:?} appears more than once"
        );
    }
}

#[tokio::test]
async fn completed_run_report_follows_the_canonical_template() {
    let f = common::fixture(Mode::Task, MockProvider::new()).await;
    f.store
        .set_run_state(&f.run_id, RunState::Completed, None)
        .await
        .unwrap();

    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert!(!export.partial);
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();

    assert_sections_in_order(&text);
    assert!(
        text.contains(&format!("report template v{REPORT_TEMPLATE_VERSION}")),
        "the footer names the template version"
    );
    assert!(
        !text.contains("**Partial result.**"),
        "a completed run is not labelled partial"
    );
}

#[tokio::test]
async fn partial_run_report_keeps_the_same_sections_and_is_labelled() {
    // The fixture run is still `running`: exporting now is the crash /
    // budget-exhaustion path, and the contract (SPEC §11) is a clearly
    // labelled partial report with the full section skeleton.
    let f = common::fixture(Mode::Task, MockProvider::new()).await;

    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert!(export.partial);
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();

    assert_sections_in_order(&text);
    assert!(
        text.contains("**Partial result.**"),
        "a partial report says so up front"
    );
}

#[tokio::test]
async fn template_version_is_recorded_in_the_manifest() {
    let f = common::fixture(Mode::Task, MockProvider::new()).await;
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let manifest: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(export.dir.join("manifest.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["report_template_version"],
        serde_json::json!(REPORT_TEMPLATE_VERSION)
    );
}

#[tokio::test]
async fn export_writes_a_pdf_twin_of_the_markdown() {
    let f = common::fixture(Mode::Task, MockProvider::new()).await;
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();

    assert!(export.files.contains(&"REPORT.md".to_string()));
    assert!(export.files.contains(&"REPORT.pdf".to_string()));

    let pdf = tokio::fs::read(export.dir.join("REPORT.pdf"))
        .await
        .unwrap();
    assert!(pdf.starts_with(b"%PDF-"), "PDF magic number");
    assert!(
        pdf.ends_with(b"%%EOF\n") || pdf.ends_with(b"%%EOF"),
        "PDF trailer terminator"
    );
    assert!(
        pdf.len() > 1_000,
        "a real report is not a stub ({} bytes)",
        pdf.len()
    );

    // The PDF is derived from the exact exported markdown, deterministically:
    // re-rendering the file on disk reproduces the same bytes.
    let markdown = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert_eq!(render_report_pdf(&markdown), pdf);
}

#[tokio::test]
async fn reexporting_unchanged_state_is_byte_for_byte_reproducible() {
    let f = common::fixture(Mode::Task, MockProvider::new()).await;
    let first = report::export_run(&f.store, &f.run_id).await.unwrap();
    let mut baseline = Vec::new();
    for name in &first.files {
        baseline.push((
            name.clone(),
            tokio::fs::read(first.dir.join(name)).await.unwrap(),
        ));
    }
    assert!(
        first.files.contains(&"REPORT.pdf".to_string()),
        "the sweep covers the canonical report files: {:?}",
        first.files
    );

    // A wall-clock export timestamp used to make the files differ.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let second = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert_eq!(second.files, first.files, "the same files are exported");
    for (name, bytes) in &baseline {
        assert_eq!(
            &tokio::fs::read(second.dir.join(name)).await.unwrap(),
            bytes,
            "{name} must be reproduced byte for byte"
        );
    }
}

#[test]
fn pdf_rendering_is_byte_deterministic() {
    let markdown = "# Determinism\n\nSame input, same bytes.\n\n- every time\n";
    assert_eq!(render_report_pdf(markdown), render_report_pdf(markdown));
}

#[test]
fn pdf_text_survives_a_round_trip_through_a_real_parser() {
    let markdown = "# Round trip\n\nThe findings mention thermal drift explicitly.\n";
    let pdf = render_report_pdf(markdown);
    let doc = pdf_text(&pdf);
    assert!(
        doc.contains("Round trip"),
        "heading text extractable: {doc}"
    );
    assert!(
        doc.contains("thermal drift"),
        "body text extractable: {doc}"
    );
}

#[test]
fn untrusted_text_cannot_break_out_of_pdf_string_literals() {
    // Generated (model-derived) text lands in PDF string literals; the
    // delimiters and escape character must not let it inject operators.
    let markdown = "# Hostile\n\nevil ) Tj ET /Root 1 0 R ( more \\ backslash (nested (parens))\n";
    let pdf = render_report_pdf(markdown);
    let doc = pdf_text(&pdf);
    assert!(
        doc.contains("evil ) Tj ET /Root 1 0 R ( more \\ backslash (nested (parens))"),
        "hostile text is rendered verbatim as text, not interpreted: {doc}"
    );
}

#[test]
fn non_winansi_characters_degrade_without_panicking() {
    let markdown = "# Unicode\n\n日本語 and emoji 🚀 degrade; café and naïve survive.\n";
    let pdf = render_report_pdf(markdown);
    let doc = pdf_text(&pdf);
    assert!(
        doc.contains("café"),
        "WinAnsi-representable text survives: {doc}"
    );
    assert!(
        !doc.contains("日本語"),
        "non-WinAnsi text is replaced: {doc}"
    );
}

#[test]
fn long_reports_paginate() {
    let mut markdown = String::from("# Long\n\n");
    for i in 0..400 {
        markdown.push_str(&format!("Paragraph {i} filling one line of the page.\n\n"));
    }
    let pdf = render_report_pdf(&markdown);
    assert!(
        page_count(&pdf) >= 2,
        "400 paragraphs do not fit one page (got {} pages)",
        page_count(&pdf)
    );
}

#[test]
fn unbroken_words_wrap_instead_of_overflowing() {
    // A single 4000-character "word" has no break points; it must be
    // hard-wrapped across lines (and pages) rather than drawn off-page.
    let markdown = format!("# Wrap\n\n{}\n", "x".repeat(4000));
    let pdf = render_report_pdf(&markdown);
    let doc = pdf_text(&pdf);
    let recovered = doc.chars().filter(|&c| c == 'x').count();
    assert!(
        recovered >= 4000,
        "all characters land on some page (got {recovered})"
    );
}

#[test]
fn empty_markdown_still_renders_a_valid_pdf() {
    let pdf = render_report_pdf("");
    assert!(pdf.starts_with(b"%PDF-"));
    assert_eq!(page_count(&pdf), 1);
}

/// Extract every page's text through the same parser the ingest tool uses.
fn pdf_text(bytes: &[u8]) -> String {
    let doc =
        pdf_oxide::PdfDocument::from_bytes(bytes.to_vec()).expect("render emits parseable PDF");
    let pages = doc.page_count().expect("page count");
    let mut out = String::new();
    for page in 0..pages {
        out.push_str(&doc.extract_text(page).unwrap_or_default());
        out.push('\n');
    }
    out
}

fn page_count(bytes: &[u8]) -> usize {
    pdf_oxide::PdfDocument::from_bytes(bytes.to_vec())
        .expect("render emits parseable PDF")
        .page_count()
        .expect("page count")
}
