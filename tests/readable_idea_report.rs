//! The human report is an explanation and reading guide, not a record dump.
mod common;

use common::*;
use tokio_util::sync::CancellationToken;
use uruk::agents::{generation, outputs::GeneratedHypothesis};
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::report;

async fn markdown(f: &Fixture) -> (String, serde_json::Value) {
    let exported = report::export_run(&f.store, &f.run_id).await.unwrap();
    let text = tokio::fs::read_to_string(exported.dir.join("REPORT.md"))
        .await
        .unwrap();
    let agent = serde_json::from_slice(
        &tokio::fs::read(exported.dir.join("AGENT_REPORT.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    (text, agent)
}

async fn seed_explained_idea(f: &Fixture, sources: Vec<SourceId>) -> ResearchItem {
    let generated: GeneratedHypothesis = serde_json::from_value(serde_json::json!({
        "title": "Measure drift with a reference channel",
        "claim": "Measure a stable reference alongside the signal so instrument drift can be separated from a real change.",
        "mechanism": "The detector can warm up and shift its readings even when the sample stays unchanged. A second channel observes a stable reference at the same time.\n\nSubtracting the reference shift from the sample reading would remove shared drift, provided both channels respond similarly to temperature. This is a proposed explanation, not a demonstrated correction.",
        "scope": "Bench measurements with a shared detector temperature; not independent instruments.",
        "assumptions": ["The reference stays stable during the measurement."],
        "predictions": ["Corrected readings should depend less on warm-up time."],
        "falsifiers": ["AUDIT-ONLY-FALSIFIER: corrected disagreement persists"],
        "validation_plan": "Connect a stable reference to the second channel, record both channels throughout warm-up, and compare corrected readings with the uncorrected baseline. No such run has been performed here.",
        "limitations": "The reference response has not been measured."
    })).unwrap();
    let parent = seed_item(f, "Archived original").await;
    f.store
        .set_disposition(&parent.id, &Disposition::Archived, Some("replaced"))
        .await
        .unwrap();
    let item = ResearchItem {
        id: ItemId::new(),
        title: generated.title.clone(),
        content: generated.to_content(),
        content_hash: ContentHash::of_str(&generated.to_content()),
        hypothesis: Some(generated.to_fields()),
        parent_ids: vec![parent.id],
        derivation: Some("AUDIT-ONLY-DERIVATION: added a reference channel".into()),
        source_ids: sources,
        ..parent
    };
    f.store.insert_item(&item).await.unwrap();
    item
}

#[tokio::test]
async fn ideas_are_explained_once_before_administrative_details() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_explained_idea(&f, vec![]).await;
    let (text, agent) = markdown(&f).await;
    let first_section = text.lines().find(|line| line.starts_with("## ")).unwrap();
    assert_eq!(first_section, "## 1. Findings");
    for prose in [
        "**What the idea is**",
        "**How it works**",
        "**What trying it would involve**",
        "**What to read**",
        "A second channel observes a stable reference at the same time.",
        "Connect a stable reference to the second channel",
        "The reference stays stable during the measurement.",
        "Corrected readings should depend less on warm-up time.",
        "The reference response has not been measured.",
    ] {
        assert!(text.contains(prose), "missing {prose}: {text}");
    }
    assert_eq!(
        text.matches("Subtracting the reference shift").count(),
        1,
        "do not duplicate the entire technical record"
    );
    for technical in [
        "AUDIT-ONLY-FALSIFIER",
        "AUDIT-ONLY-DERIVATION",
        "## Claim\n",
        "**Falsifiers:**",
        "**What changed:**",
        "### Candidates\n",
        "Epistemic status",
    ] {
        assert!(
            !text.contains(technical),
            "technical audit detail leaked: {technical}"
        );
    }
    let record = agent["research_records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == item.id.as_str())
        .unwrap();
    assert!(
        record["hypothesis"]["falsifiers"][0]
            .as_str()
            .unwrap()
            .contains("AUDIT-ONLY-FALSIFIER")
    );
    assert!(
        record["derivation"]
            .as_str()
            .unwrap()
            .contains("AUDIT-ONLY-DERIVATION")
    );
    assert_eq!(agent["human_report_markdown"], text);
}

#[tokio::test]
async fn the_five_ideas_follow_elo_not_creation_order() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let mut expected: Vec<(f64, ResearchItem)> = Vec::new();
    for (title, rating) in [
        ("Third", 1300.0),
        ("Fifth", 1100.0),
        ("Winner", 1550.0),
        ("Sixth", 1000.0),
        ("Second", 1400.0),
        ("Fourth", 1200.0),
    ] {
        let item = seed_item(&f, title).await;
        f.store.ensure_rating(&item.id, &f.plan.id).await.unwrap();
        sqlx::query(
            "UPDATE ratings SET rating = ?, matches_played = 4 WHERE item_id = ? AND rubric_id = ?",
        )
        .bind(rating)
        .bind(item.id.as_str())
        .bind(f.plan.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
        expected.push((rating, item));
    }
    expected.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (text, agent) = markdown(&f).await;
    let wanted: Vec<_> = expected
        .iter()
        .take(5)
        .map(|(_, item)| item.id.clone())
        .collect();
    assert_eq!(agent["top_finding_ids"], serde_json::json!(wanted));
    let top = text
        .split("### Top 5 ideas by Elo\n")
        .nth(1)
        .expect("plain-language ranked ideas")
        .split(report::REPORT_SECTIONS[1])
        .next()
        .unwrap();
    for (rank, (_, item)) in expected.iter().take(5).enumerate() {
        assert!(
            top.contains(&format!("#### {}. {}", rank + 1, item.title)),
            "{top}"
        );
    }
    assert!(!top.contains("Sixth"));
    assert_eq!(top.matches("#### ").count(), 5);
}

#[tokio::test]
async fn each_idea_links_its_own_reading_with_title_access_and_no_invented_relevance() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let abstract_source = seed_source_adjusted(
        &f,
        "Reference-channel study",
        "Synthetic abstract fixture.",
        |s| {
            s.origin = Origin::Url("https://example.org/reference-study".into());
            s.access = AccessLevel::AbstractOnly;
            s.access_limitations = Some("Full text is paywalled".into());
        },
    )
    .await;
    let full_source = seed_source_adjusted(
        &f,
        "Calibration handbook",
        "Synthetic handbook fixture.",
        |s| {
            s.origin = Origin::Url("https://example.org/calibration-handbook".into());
        },
    )
    .await;
    let unrelated = seed_source(&f, "Unrelated notebook", "A different idea.").await;
    seed_explained_idea(
        &f,
        vec![abstract_source.id, full_source.id.clone(), full_source.id],
    )
    .await;
    let (text, _) = markdown(&f).await;
    let top = text
        .split("### Top 5 ideas by Elo\n")
        .nth(1)
        .expect("ranked ideas")
        .split(report::REPORT_SECTIONS[1])
        .next()
        .unwrap();
    assert!(
        top.contains("[Calibration handbook](<https://example.org/calibration-handbook>)"),
        "{top}"
    );
    assert!(
        top.contains("[Reference-channel study](<https://example.org/reference-study>)"),
        "{top}"
    );
    assert!(top.contains("Start here"));
    assert!(
        top.find("Calibration handbook").unwrap() < top.find("Reference-channel study").unwrap(),
        "start with readable full text"
    );
    assert_eq!(
        top.matches("[Calibration handbook]").count(),
        1,
        "deduplicate source references"
    );
    assert!(top.contains("Only the abstract was available"));
    assert!(top.contains("[View this source in Urukai](/runs/"));
    assert!(!top.contains("Read the saved text in Urukai"));
    assert!(top.contains("Full text is paywalled"));
    assert!(!top.contains(unrelated.title.as_ref().unwrap()));
    assert!(
        top.contains("No explanation of its relevance was recorded"),
        "do not manufacture an annotation from the title"
    );
}

#[tokio::test]
async fn generated_reading_reasons_and_locators_survive_persistence_into_the_report() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let source = seed_source_adjusted(
        &f,
        "Reference design notes",
        "Synthetic design notes fixture.",
        |s| {
            s.origin = Origin::Url("https://example.org/reference-design".into());
        },
    )
    .await;
    let mut reply: serde_json::Value = serde_json::from_str(&hypothesis_json(
        "Reference design",
        "Use a parallel reference",
    ))
    .unwrap();
    reply["grounding"] = serde_json::json!([{
        "source_id": source.id, "locator": "Section 3, reference-channel wiring",
        "supports": "Explains how to connect the reference channel and why the two paths need matching temperature response."
    }]);
    let f = Fixture {
        provider: std::sync::Arc::new(MockProvider::new().default_reply(reply.to_string())),
        ..f
    };
    let ctx = context(&f, CancellationToken::new());
    let generated = generation::from_literature(&ctx, None, None).await.unwrap();
    f.store.insert_item(&generated.item).await.unwrap();
    let (text, _) = markdown(&f).await;
    assert!(
        text.contains("Section 3, reference-channel wiring"),
        "the generation locator must not be discarded: {text}"
    );
    assert!(
        text.contains("Explains how to connect the reference channel"),
        "the reading reason must be persisted: {text}"
    );
}

#[tokio::test]
async fn a_task_without_ranked_ideas_still_explains_its_observations() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    f.store
        .insert_evidence(&Evidence {
            id: EvidenceId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: f.run_id.clone(),
            kind: EvidenceKind::SuppliedObservation,
            content: "The measured ratio was higher in the supplied comparison.".into(),
            method: "A paired measurement in the supplied dataset.".into(),
            inputs: vec![],
            source_ids: vec![],
            artifact_ids: vec![],
            limitations: Some("One dataset; not an independent replication.".into()),
            produced_by: None,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .unwrap();
    let (text, _) = markdown(&f).await;
    assert!(
        text.contains("The measured ratio was higher"),
        "the human report lost the standalone task result: {text}"
    );
    assert!(text.contains("A paired measurement"));
    assert!(text.contains("One dataset; not an independent replication."));
    assert!(!text.contains("No findings were produced"));
}

#[tokio::test]
async fn synthesis_claims_use_readable_source_titles_instead_of_internal_ids() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let source = seed_source_adjusted(&f, "Measurement guide", "Synthetic guide.", |s| {
        s.origin = Origin::Url("https://example.org/measurement-guide".into());
    })
    .await;
    seed_deliverable(&f, &serde_json::json!({
        "title": "Measurement explanation", "body": "A supplied guide explains the comparison.",
        "claims": [{"claim": "The guide describes a paired comparison.", "basis": "source_reported",
            "citations": [{"source_id": source.id, "locator": "Chapter 2"}]}],
        "disagreements": [], "limitations": "One supplied guide.", "next_actions": []
    })).await;
    let (text, _) = markdown(&f).await;
    let findings = text.split(report::REPORT_SECTIONS[1]).next().unwrap();
    assert!(
        findings.contains("[Measurement guide](<https://example.org/measurement-guide>)"),
        "{findings}"
    );
    assert!(findings.contains("Chapter 2"));
    assert!(
        !findings.contains(source.id.as_str()),
        "internal citation IDs are for the agent export"
    );
}

#[tokio::test]
async fn ranked_ideas_do_not_repeat_a_technical_or_out_of_order_synthesis() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let idea = seed_explained_idea(&f, vec![]).await;
    let content = "AUDIT-ONLY-SYNTHESIS: an unranked recap with falsifiers and changes";
    let synthesis = ResearchItem {
        id: ItemId::new(),
        kind: ItemKind::Synthesis,
        title: "Campaign recap".into(),
        content: content.into(),
        content_hash: ContentHash::of_str(content),
        hypothesis: None,
        ..idea
    };
    f.store.insert_item(&synthesis).await.unwrap();
    let (text, agent) = markdown(&f).await;
    assert!(
        !text.contains(content),
        "the human report should explain each ranked idea once"
    );
    assert!(
        agent["research_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["content"] == content)
    );
}

#[test]
fn historical_hypothesis_fields_remain_readable_without_new_annotations() {
    let old = serde_json::json!({"claim": "Recorded claim", "mechanism": "Recorded mechanism",
        "assumptions": [], "scope": "Bench", "predictions": [], "falsifiers": [],
        "validation_plan": "Compare both measurements"});
    let fields: HypothesisFields = serde_json::from_value(old).unwrap();
    assert!(fields.reading.is_empty());
    assert!(fields.limitations.is_empty());
    assert_eq!(fields.claim, "Recorded claim");
}

#[test]
fn idea_and_synthesis_prompts_request_explanations_for_humans() {
    let registry = uruk::prompts::PromptRegistry::new();
    for id in [
        "generation.literature",
        "generation.debate",
        "evolution.feasibility",
        "evolution.analogy",
        "task.synthesis",
    ] {
        let template = registry.get(id).unwrap();
        assert!(
            template.text.contains("plain English"),
            "{id} still addresses only domain experts"
        );
        assert!(
            template.text.contains("step by step"),
            "{id} does not request an actual explanation"
        );
        assert!(
            template.text.contains("what to read"),
            "{id} omits reading guidance"
        );
    }
}

#[tokio::test]
async fn missing_reading_and_unsafe_source_urls_are_not_fabricated_or_activated() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let source = seed_source_adjusted(&f, "Unsafe [label]", "Synthetic text fixture.", |s| {
        s.origin = Origin::Url("javascript:alert(1)".into());
        s.access = AccessLevel::Unavailable;
        s.text_artifact = None;
    })
    .await;
    seed_explained_idea(&f, vec![source.id, SourceId::from_raw("src_missing")]).await;
    let (text, _) = markdown(&f).await;
    assert!(!text.contains("](javascript:"));
    assert!(!text.contains("](<javascript:"));
    assert!(text.contains("Could not be read"), "{text}");
    assert!(
        text.contains("No public reading link was recorded"),
        "{text}"
    );
    assert!(!text.contains("[src_missing]"));
}
