mod common;

use common::*;
use time::OffsetDateTime;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::web::view::run_snapshot;

#[tokio::test]
async fn badges_name_only_the_evidence_of_the_current_assessment() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_item(&f, "Measured drift").await;
    let source = seed_source(&f, "Independent temperature study", "Measured drift.").await;
    let review = seed_review(&f, &item).await;
    let evidence = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: EvidenceKind::RetrievedLiterature,
        content: "Temperature tracks drift".into(),
        method: "source-grounded review".into(),
        inputs: vec![],
        source_ids: vec![source.id],
        artifact_ids: vec![],
        limitations: Some("One instrument".into()),
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_evidence(&evidence).await.unwrap();
    let mut assessment = AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Supported,
        basis_review: Some(review.id.clone()),
        basis_decision: None,
        evidence: vec![EvidenceLink {
            evidence_id: evidence.id.clone(),
            item_id: item.id.clone(),
            claim: "Drift tracks temperature".into(),
            relation: EvidenceRelation::Supports,
            justification: "The measured shift agrees".into(),
            limits: Some("One instrument".into()),
        }],
        rationale: "Supported only for the measured instrument".into(),
        assessed_at: OffsetDateTime::now_utc(),
    };
    f.store.record_assessment(&assessment).await.unwrap();
    let view = serde_json::to_value(run_snapshot(&f.store, &f.run_id).await.unwrap()).unwrap();
    let card = &view["items"][0];
    assert_eq!(card["assessment"], "supported");
    assert_eq!(card["assessment_basis"]["review_id"], review.id.as_str());
    assert_eq!(card["assessment_basis"]["rationale"], assessment.rationale);
    assert_eq!(
        card["assessment_basis"]["evidence"][0]["id"],
        evidence.id.as_str()
    );
    assert_eq!(
        card["assessment_basis"]["evidence"][0]["label"],
        "Independent temperature study"
    );
    assert_eq!(
        card["assessment_basis"]["evidence"][0]["relation"],
        "supports"
    );

    assessment.assessment = Assessment::Inconclusive;
    assessment.rationale = "Later controls do not resolve the mechanism".into();
    assessment.evidence.clear();
    f.store.record_assessment(&assessment).await.unwrap();
    let view = serde_json::to_value(run_snapshot(&f.store, &f.run_id).await.unwrap()).unwrap();
    assert_eq!(view["items"][0]["assessment"], "inconclusive");
    assert_eq!(
        view["items"][0]["assessment_basis"]["evidence"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn untested_is_explicit_and_agent_opinion_cannot_create_a_supported_badge() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_item(&f, "Untested explanation").await;
    let review = seed_review(&f, &item).await;
    let assessment = AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Supported,
        basis_review: Some(review.id),
        basis_decision: None,
        evidence: vec![],
        rationale: "The agent believes it".into(),
        assessed_at: OffsetDateTime::now_utc(),
    };
    assert!(f.store.record_assessment(&assessment).await.is_err());
    let view = serde_json::to_value(run_snapshot(&f.store, &f.run_id).await.unwrap()).unwrap();
    assert_eq!(view["items"][0]["assessment"], "untested");
    assert!(
        view["items"][0]
            .as_object()
            .unwrap()
            .contains_key("assessment_basis")
    );
    assert!(view["items"][0]["assessment_basis"].is_null());
}
