//! Human explanation and machine-readable audit data share one persisted run.
mod common;

use common::*;
use time::OffsetDateTime;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::report;

#[tokio::test]
async fn human_report_explains_only_the_top_five_active_candidates() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let mut items = Vec::new();
    for i in 0..7 {
        items.push(seed_item(&f, &format!("Candidate {i}")).await);
    }
    // Only this candidate is rated: it must sort above all unrated items.
    let leader = &items[6];
    f.store.ensure_rating(&leader.id, &f.plan.id).await.unwrap();
    f.store
        .set_disposition(&items[0].id, &Disposition::Archived, Some("retired"))
        .await
        .unwrap();
    seed_review(&f, leader).await;
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let markdown = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    let top = markdown
        .split("### Top 5 findings\n")
        .nth(1)
        .expect("top-five section")
        .split("### Candidates\n")
        .next()
        .unwrap();
    assert_eq!(top.matches("#### ").count(), 5);
    assert!(top.contains(&format!("#### 1. {}", leader.title)));
    assert!(!top.contains("Candidate 0"));
    assert!(top.contains(&leader.hypothesis.as_ref().unwrap().claim));
    assert!(top.contains("stated mechanism"));
    assert!(top.contains("**Evidence and limits**"));
    assert!(top.contains("**Review findings and objections**"));
    assert!(top.contains("**Next test**"));
    assert!(top.contains("untested"));
    assert!(top.contains("not a probability"));
}

#[tokio::test]
async fn fewer_than_five_findings_are_labelled_without_inventing_more() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    seed_item(&f, "Only recorded candidate").await;
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let markdown = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(markdown.contains("Only 1 active candidate(s) were recorded"));
    assert!(markdown.contains("#### 1. Only recorded candidate"));
    assert!(!markdown.contains("#### 2."));
    assert!(markdown.contains("Partial result"));
}

#[tokio::test]
async fn agent_report_preserves_structured_records_and_the_exact_human_report() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_item(&f, "Traceable candidate").await;
    seed_review(&f, &item).await;
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert!(export.files.contains(&"AGENT_REPORT.json".into()));
    let bytes = tokio::fs::read(export.dir.join("AGENT_REPORT.json"))
        .await
        .unwrap();
    let agent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(agent["format_version"], 1);
    assert_eq!(agent["manifest"]["run_id"], f.run_id.as_str());
    assert_eq!(agent["manifest"]["partial"], true);
    assert!(agent["manifest"]["usage"]["cost_usd"].is_null());
    assert_eq!(agent["top_finding_ids"], serde_json::json!([item.id]));
    assert!(
        agent["research_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == item.id.as_str())
    );
    let markdown = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert_eq!(agent["human_report_markdown"], markdown);
    report::export_run(&f.store, &f.run_id).await.unwrap();
    assert_eq!(
        tokio::fs::read(export.dir.join("AGENT_REPORT.json"))
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(
        f.store.budget_usage(&f.run_id).await.unwrap().model_calls,
        0
    );
}

#[tokio::test]
async fn current_assessment_links_are_explained_even_when_the_original_link_was_context() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_item(&f, "Measured candidate").await;
    let source = seed_source(&f, "Recorded study", "Measured data").await;
    let mut review = seed_review(&f, &item).await;
    review.id = ReviewId::new();
    review.objections.push(Objection {
        description: "Only one condition was measured".into(),
        fatal: false,
        targets: Some("generalisability".into()),
        suggested_check: Some("Repeat at a second condition".into()),
    });
    review
        .unknowns
        .push("Cross-condition effect is unknown".into());
    review
        .next_actions
        .push("Run the second-condition check".into());
    f.store.insert_review(&review).await.unwrap();
    let evidence = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: EvidenceKind::SuppliedObservation,
        content: "A recorded measurement".into(),
        method: "Recorded measurement protocol".into(),
        inputs: vec![],
        source_ids: vec![source.id],
        artifact_ids: vec![],
        limitations: Some("A single-condition measurement".into()),
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_evidence(&evidence).await.unwrap();
    let context = EvidenceLink {
        evidence_id: evidence.id,
        item_id: item.id.clone(),
        claim: "measured scoped claim".into(),
        relation: EvidenceRelation::Context,
        justification: "Not interpreted yet".into(),
        limits: None,
    };
    f.store.link_evidence(&context).await.unwrap();
    let supporting = EvidenceLink {
        relation: EvidenceRelation::Supports,
        justification: "The measurement matches the scoped prediction".into(),
        limits: Some("No out-of-condition inference".into()),
        ..context
    };
    f.store
        .record_assessment(&AssessmentRecord {
            item_id: item.id,
            assessment: Assessment::Supported,
            basis_review: Some(review.id),
            basis_decision: None,
            evidence: vec![supporting],
            rationale: "Interpreted the scoped measurement".into(),
            assessed_at: OffsetDateTime::now_utc(),
        })
        .await
        .unwrap();
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let markdown = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    let top = markdown
        .split("### Top 5 findings\n")
        .nth(1)
        .unwrap()
        .split("### Candidates\n")
        .next()
        .unwrap();
    assert!(
        top.contains("cited by the current assessment): **supports**"),
        "{top}"
    );
    for text in [
        "Interpreted the scoped measurement",
        "No out-of-condition inference",
        "A single-condition measurement",
        "Only one condition was measured",
        "Repeat at a second condition",
        "Cross-condition effect is unknown",
        "Run the second-condition check",
        "recorded context, not a basis",
    ] {
        assert!(top.contains(text), "missing {text}: {top}");
    }
    assert!(top.contains("**Current status: supported.**"));
}
