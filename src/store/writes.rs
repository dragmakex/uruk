//! The controlled write path (SPEC §6, §9.2).
//!
//! Every scientific record enters through here. Agents propose; this module
//! validates and commits. Assessment writes pass the §4.3 gate, match writes
//! apply an Elo update exactly once, and a task's records are committed with
//! its outcome and settlement in one transaction through [`RecordBatch`].
//!
//! Each public method wraps an `*_in_tx` function so the same validation runs
//! whether a record is written alone (CLI, tests) or as part of a batch.

use super::{Store, now, to_rfc3339};
use crate::records::*;
use crate::{Error, Result};
use sqlx::{Row, SqliteConnection};

/// Records a task proposes, committed atomically with its outcome (SPEC §9.2).
///
/// Order of application matters: items before dispositions and reviews,
/// evidence before links and assessments, reviews before the assessments that
/// cite them.
#[derive(Debug, Clone, Default)]
pub struct RecordBatch {
    pub items: Vec<ResearchItem>,
    pub dispositions: Vec<(ItemId, Disposition, Option<String>)>,
    pub evidence: Vec<Evidence>,
    pub links: Vec<EvidenceLink>,
    pub reviews: Vec<Review>,
    pub assessments: Vec<AssessmentRecord>,
    pub decisions: Vec<Decision>,
    pub artifacts: Vec<Artifact>,
    pub experiments: Vec<Experiment>,
    pub clusters: Vec<Cluster>,
    pub feedback: Vec<Feedback>,
    /// Cohort enrolments at the initial rating.
    pub ratings: Vec<(ItemId, PlanId)>,
    /// Matches with their dedupe keys.
    pub matches: Vec<(Match, String)>,
}

impl RecordBatch {
    /// IDs of the records this batch commits, for the task's `output_refs`.
    pub fn output_refs(&self) -> Vec<String> {
        let mut refs = Vec::new();
        refs.extend(self.items.iter().map(|i| i.id.0.clone()));
        refs.extend(self.evidence.iter().map(|e| e.id.0.clone()));
        refs.extend(self.reviews.iter().map(|r| r.id.0.clone()));
        refs.extend(self.decisions.iter().map(|d| d.id.0.clone()));
        refs.extend(self.artifacts.iter().map(|a| a.id.0.clone()));
        refs.extend(self.experiments.iter().map(|e| e.id.0.clone()));
        refs.extend(self.clusters.iter().map(|c| c.id.0.clone()));
        refs.extend(self.feedback.iter().map(|f| f.id.0.clone()));
        refs.extend(self.matches.iter().map(|(m, _)| m.id.0.clone()));
        refs
    }

    /// Keep only what is safe to retain from a task that did not finish:
    /// labelled artifacts (transcripts, logs) and decisions. Half-built
    /// scientific records are dropped rather than committed inconsistently.
    pub fn retain_partial(&mut self) {
        let artifacts = std::mem::take(&mut self.artifacts);
        let decisions = std::mem::take(&mut self.decisions);
        *self = Self {
            artifacts,
            decisions,
            ..Default::default()
        };
    }

    pub fn is_empty(&self) -> bool {
        self.output_refs().is_empty() && self.dispositions.is_empty() && self.links.is_empty()
    }

    /// Apply every record in dependency order.
    pub(crate) async fn apply(&self, conn: &mut SqliteConnection) -> Result<()> {
        for item in &self.items {
            insert_item_in_tx(conn, item).await?;
        }
        for (id, disposition, reason) in &self.dispositions {
            set_disposition_in_tx(conn, id, disposition, reason.as_deref()).await?;
        }
        for e in &self.evidence {
            insert_evidence_in_tx(conn, e).await?;
        }
        for l in &self.links {
            link_evidence_in_tx(conn, l).await?;
        }
        for r in &self.reviews {
            insert_review_in_tx(conn, r).await?;
        }
        for a in &self.assessments {
            record_assessment_in_tx(conn, a).await?;
        }
        for d in &self.decisions {
            insert_decision_in_tx(conn, d).await?;
        }
        for a in &self.artifacts {
            insert_artifact_in_tx(conn, a).await?;
        }
        for e in &self.experiments {
            insert_experiment_in_tx(conn, e).await?;
        }
        for c in &self.clusters {
            insert_cluster_in_tx(conn, c).await?;
        }
        for f in &self.feedback {
            insert_feedback_in_tx(conn, f).await?;
        }
        for (item, rubric) in &self.ratings {
            super::queries::ensure_rating_in_tx(conn, item, rubric).await?;
        }
        for (m, key) in &self.matches {
            super::queries::commit_match_in_tx(conn, m, key, DEFAULT_K).await?;
        }
        Ok(())
    }
}

impl Store {
    /// Create a project row if it does not already exist.
    pub async fn ensure_project(&self, id: &ProjectId, name: &str) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query("INSERT OR IGNORE INTO projects (id, name, created_at) VALUES (?, ?, ?)")
            .bind(id.as_str())
            .bind(name)
            .bind(to_rfc3339(now()))
            .execute(guard.conn())
            .await?;
        guard.commit().await
    }

    /// Insert a run together with its first goal revision, atomically.
    pub async fn create_run(&self, run: &Run, goal: &Goal) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let ts = to_rfc3339(run.created_at);

        sqlx::query(
            "INSERT INTO runs (id, schema_version, project_id, goal_id, plan_id, state,
                               stop_condition, iterations, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run.id.as_str())
        .bind(run.schema_version)
        .bind(run.project_id.as_str())
        .bind(run.goal_id.as_str())
        .bind(run.plan_id.as_ref().map(|p| p.0.clone()))
        .bind(run.state.as_str())
        .bind(run.stop_condition.as_ref().map(|s| s.as_str()))
        .bind(run.iterations)
        .bind(&ts)
        .bind(&ts)
        .execute(guard.conn())
        .await?;

        insert_goal(guard.conn(), goal).await?;
        guard.commit().await
    }

    /// Record a new goal revision and point the run at it (SPEC §4.1).
    ///
    /// Plan revisions must invalidate stale approvals, so pending approvals
    /// under the previous revision are marked `Invalidated` (SPEC §6).
    pub async fn revise_goal(&self, goal: &Goal) -> Result<usize> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        insert_goal(conn, goal).await?;

        sqlx::query("UPDATE runs SET goal_id = ?, updated_at = ? WHERE id = ?")
            .bind(goal.id.as_str())
            .bind(to_rfc3339(now()))
            .bind(goal.run_id.as_str())
            .execute(&mut *conn)
            .await?;

        // The row's `state` column and the serialized body must not disagree,
        // or a reader that deserializes `body` would see a stale "pending".
        let pending: Vec<(String, String)> =
            sqlx::query_as("SELECT id, body FROM approvals WHERE run_id = ? AND state = 'pending'")
                .bind(goal.run_id.as_str())
                .fetch_all(&mut *conn)
                .await?;

        let invalidated = pending.len();
        for (id, body) in pending {
            let mut req: ApprovalRequest = serde_json::from_str(&body)?;
            req.state = ApprovalState::Invalidated;
            req.decided_reason = Some(format!(
                "goal revised to {} ({}); a changed request needs a new approval",
                goal.id, goal.revision
            ));
            sqlx::query("UPDATE approvals SET state = ?, body = ? WHERE id = ?")
                .bind(ApprovalState::Invalidated.as_str())
                .bind(serde_json::to_string(&req)?)
                .bind(&id)
                .execute(&mut *conn)
                .await?;
        }

        guard.commit().await?;
        Ok(invalidated)
    }

    pub async fn insert_plan(&self, plan: &Plan) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        sqlx::query(
            "INSERT INTO plans (id, run_id, goal_id, revision, body, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(plan.id.as_str())
        .bind(plan.run_id.as_str())
        .bind(plan.goal_id.as_str())
        .bind(plan.revision)
        .bind(serde_json::to_string(plan)?)
        .bind(to_rfc3339(plan.created_at))
        .execute(&mut *conn)
        .await?;

        sqlx::query("UPDATE runs SET plan_id = ?, updated_at = ? WHERE id = ?")
            .bind(plan.id.as_str())
            .bind(to_rfc3339(now()))
            .bind(plan.run_id.as_str())
            .execute(&mut *conn)
            .await?;

        guard.commit().await
    }

    pub async fn set_run_state(
        &self,
        run_id: &RunId,
        state: RunState,
        stop: Option<StopCondition>,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query(
            "UPDATE runs SET state = ?, stop_condition = COALESCE(?, stop_condition),
                                 updated_at = ? WHERE id = ?",
        )
        .bind(state.as_str())
        .bind(stop.as_ref().map(|s| s.as_str()))
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .execute(guard.conn())
        .await?;
        guard.commit().await
    }

    /// Reopen a finished run after a goal revision, so it can be resumed
    /// under the new revision (SPEC §6: the researcher may change constraints
    /// at any point).
    pub async fn reopen_run(&self, run_id: &RunId) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query(
            "UPDATE runs SET state = 'running', stop_condition = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .execute(guard.conn())
        .await?;
        guard.commit().await
    }

    pub async fn bump_iteration(&self, run_id: &RunId) -> Result<u32> {
        let mut guard = self.begin_write().await?;
        let row = sqlx::query(
            "UPDATE runs SET iterations = iterations + 1, updated_at = ?
             WHERE id = ? RETURNING iterations",
        )
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .fetch_one(guard.conn())
        .await?;
        guard.commit().await?;
        Ok(row.get::<i64, _>("iterations") as u32)
    }

    pub async fn insert_source(&self, source: &Source) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query(
            "INSERT INTO sources (id, run_id, content_hash, access, body, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(source.id.as_str())
        .bind(source.run_id.as_str())
        .bind(source.content_hash.as_str())
        .bind(source.access.as_str())
        .bind(serde_json::to_string(source)?)
        .bind(to_rfc3339(source.retrieved_at))
        .execute(guard.conn())
        .await?;
        guard.commit().await
    }

    /// Rewrite a source's record in place (acquisition backfills
    /// bibliographic fields extraction could not find). The denormalized
    /// columns are kept in sync with the body.
    pub async fn update_source(&self, source: &Source) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query("UPDATE sources SET content_hash = ?, access = ?, body = ? WHERE id = ?")
            .bind(source.content_hash.as_str())
            .bind(source.access.as_str())
            .bind(serde_json::to_string(source)?)
            .bind(source.id.as_str())
            .execute(guard.conn())
            .await?;
        guard.commit().await
    }

    pub async fn insert_search_record(&self, rec: &SearchRecord) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query("INSERT INTO search_records (run_id, body, created_at) VALUES (?, ?, ?)")
            .bind(rec.run_id.as_str())
            .bind(serde_json::to_string(rec)?)
            .bind(to_rfc3339(rec.executed_at))
            .execute(guard.conn())
            .await?;
        guard.commit().await
    }

    /// Insert an immutable item plus its parent links and initial state.
    pub async fn insert_item(&self, item: &ResearchItem) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_item_in_tx(guard.conn(), item).await?;
        guard.commit().await
    }

    /// Change an item's workflow disposition (SPEC §4.3).
    pub async fn set_disposition(
        &self,
        item_id: &ItemId,
        disposition: &Disposition,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        set_disposition_in_tx(guard.conn(), item_id, disposition, reason).await?;
        guard.commit().await
    }

    pub async fn insert_evidence(&self, evidence: &Evidence) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_evidence_in_tx(guard.conn(), evidence).await?;
        guard.commit().await
    }

    /// Link evidence to a claim and mark dependent ratings stale (SPEC §7).
    pub async fn link_evidence(&self, link: &EvidenceLink) -> Result<()> {
        let mut guard = self.begin_write().await?;
        link_evidence_in_tx(guard.conn(), link).await?;
        guard.commit().await
    }

    pub async fn insert_review(&self, review: &Review) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_review_in_tx(guard.conn(), review).await?;
        guard.commit().await
    }

    /// Record an epistemic assessment, enforcing the §4.3 gate.
    ///
    /// This is the only path by which an item becomes `Supported` or
    /// `Refuted`, so agent opinion cannot promote itself.
    pub async fn record_assessment(&self, record: &AssessmentRecord) -> Result<()> {
        let mut guard = self.begin_write().await?;
        record_assessment_in_tx(guard.conn(), record).await?;
        guard.commit().await
    }

    pub async fn insert_decision(&self, decision: &Decision) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_decision_in_tx(guard.conn(), decision).await?;
        guard.commit().await
    }

    pub async fn insert_artifact(&self, artifact: &Artifact) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_artifact_in_tx(guard.conn(), artifact).await?;
        guard.commit().await
    }

    pub async fn insert_experiment(&self, exp: &Experiment) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_experiment_in_tx(guard.conn(), exp).await?;
        guard.commit().await
    }

    pub async fn insert_cluster(&self, cluster: &Cluster) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_cluster_in_tx(guard.conn(), cluster).await?;
        guard.commit().await
    }

    pub async fn insert_feedback(&self, feedback: &Feedback) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_feedback_in_tx(guard.conn(), feedback).await?;
        guard.commit().await
    }

    pub async fn insert_snapshot(&self, run_id: &RunId, snap: &CampaignSnapshot) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query(
            "INSERT INTO snapshots (run_id, iteration, body, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(run_id.as_str())
        .bind(snap.iteration)
        .bind(serde_json::to_string(snap)?)
        .bind(to_rfc3339(now()))
        .execute(guard.conn())
        .await?;
        guard.commit().await
    }

    /// Record or replay a nondeterministic scheduling choice (SPEC §6).
    ///
    /// "Persist sampled choices rather than drawing fresh randomness on
    /// resume." Returns the stored choice when one already exists.
    pub async fn record_or_replay_choice(
        &self,
        run_id: &RunId,
        iteration: u32,
        key: &str,
        drawn: &str,
    ) -> Result<String> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        if let Some(existing) = sqlx::query_scalar::<_, String>(
            "SELECT choice FROM scheduler_choices
             WHERE run_id = ? AND iteration = ? AND choice_key = ?",
        )
        .bind(run_id.as_str())
        .bind(iteration)
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?
        {
            return Ok(existing);
        }

        sqlx::query(
            "INSERT INTO scheduler_choices (run_id, iteration, choice_key, choice, created_at)
                 VALUES (?, ?, ?, ?, ?)",
        )
        .bind(run_id.as_str())
        .bind(iteration)
        .bind(key)
        .bind(drawn)
        .bind(to_rfc3339(now()))
        .execute(&mut *conn)
        .await?;
        guard.commit().await?;
        Ok(drawn.to_string())
    }
}

async fn insert_goal(conn: &mut SqliteConnection, goal: &Goal) -> Result<()> {
    sqlx::query(
        "INSERT INTO goals (id, run_id, revision, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(goal.id.as_str())
    .bind(goal.run_id.as_str())
    .bind(goal.revision)
    .bind(serde_json::to_string(goal)?)
    .bind(to_rfc3339(goal.created_at))
    .execute(conn)
    .await?;
    Ok(())
}

/// Rejects a child that tries to reuse an existing item ID, which is how a
/// caller would otherwise mutate a ranked parent (SPEC §5 Evolution).
pub(crate) async fn insert_item_in_tx(
    conn: &mut SqliteConnection,
    item: &ResearchItem,
) -> Result<()> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM items WHERE id = ?")
        .bind(item.id.as_str())
        .fetch_optional(&mut *conn)
        .await?;
    if exists.is_some() {
        return Err(Error::validation(format!(
            "item {} already exists; items are immutable, create a child instead (SPEC §4.2)",
            item.id
        )));
    }

    sqlx::query(
        "INSERT INTO items (id, run_id, kind, goal_id, content_hash, body, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(item.id.as_str())
    .bind(item.run_id.as_str())
    .bind(item.kind.as_str())
    .bind(item.goal_id.as_str())
    .bind(item.content_hash.as_str())
    .bind(serde_json::to_string(item)?)
    .bind(to_rfc3339(item.created_at))
    .execute(&mut *conn)
    .await?;

    for parent in &item.parent_ids {
        sqlx::query("INSERT INTO item_parents (child_id, parent_id) VALUES (?, ?)")
            .bind(item.id.as_str())
            .bind(parent.as_str())
            .execute(&mut *conn)
            .await?;
    }

    sqlx::query(
        "INSERT INTO item_states (item_id, disposition, duplicate_of, reason, updated_at)
         VALUES (?, 'active', NULL, NULL, ?)",
    )
    .bind(item.id.as_str())
    .bind(to_rfc3339(item.created_at))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Disposition never touches the item's content or its assessment.
pub(crate) async fn set_disposition_in_tx(
    conn: &mut SqliteConnection,
    item_id: &ItemId,
    disposition: &Disposition,
    reason: Option<&str>,
) -> Result<()> {
    let duplicate_of = match disposition {
        Disposition::Duplicate(rep) => {
            if rep == item_id {
                return Err(Error::validation("an item cannot duplicate itself"));
            }
            Some(rep.0.clone())
        }
        _ => None,
    };
    sqlx::query(
        "UPDATE item_states SET disposition = ?, duplicate_of = ?, reason = ?, updated_at = ?
             WHERE item_id = ?",
    )
    .bind(disposition.as_str())
    .bind(duplicate_of)
    .bind(reason)
    .bind(to_rfc3339(now()))
    .bind(item_id.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn insert_evidence_in_tx(
    conn: &mut SqliteConnection,
    evidence: &Evidence,
) -> Result<()> {
    sqlx::query("INSERT INTO evidence (id, run_id, kind, body, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(evidence.id.as_str())
        .bind(evidence.run_id.as_str())
        .bind(evidence.kind.as_str())
        .bind(serde_json::to_string(evidence)?)
        .bind(to_rfc3339(evidence.created_at))
        .execute(conn)
        .await?;
    Ok(())
}

/// "New evidence makes dependent assessments/comparisons candidates for
/// re-review... visibly mark stale rankings until refreshed."
pub(crate) async fn link_evidence_in_tx(
    conn: &mut SqliteConnection,
    link: &EvidenceLink,
) -> Result<()> {
    let known: Option<String> = sqlx::query_scalar("SELECT id FROM evidence WHERE id = ?")
        .bind(link.evidence_id.as_str())
        .fetch_optional(&mut *conn)
        .await?;
    if known.is_none() {
        return Err(Error::not_found(format!(
            "link cites unknown evidence {}",
            link.evidence_id
        )));
    }

    sqlx::query(
        "INSERT INTO evidence_links (evidence_id, item_id, relation, claim, body, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(link.evidence_id.as_str())
    .bind(link.item_id.as_str())
    .bind(link.relation.as_str())
    .bind(&link.claim)
    .bind(serde_json::to_string(link)?)
    .bind(to_rfc3339(now()))
    .execute(&mut *conn)
    .await?;

    // Only evidence that bears on the claim's truth invalidates a ranking.
    if matches!(
        link.relation,
        EvidenceRelation::Supports | EvidenceRelation::Contradicts
    ) {
        sqlx::query("UPDATE ratings SET stale = 1, updated_at = ? WHERE item_id = ?")
            .bind(to_rfc3339(now()))
            .bind(link.item_id.as_str())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// A review must target an item version that actually exists, and its
/// recorded hash must match, so a stale review is detectable.
pub(crate) async fn insert_review_in_tx(
    conn: &mut SqliteConnection,
    review: &Review,
) -> Result<()> {
    let hash: Option<String> = sqlx::query_scalar("SELECT content_hash FROM items WHERE id = ?")
        .bind(review.item_id.as_str())
        .fetch_optional(&mut *conn)
        .await?;
    let Some(hash) = hash else {
        return Err(Error::not_found(format!(
            "review targets unknown item {}",
            review.item_id
        )));
    };
    if hash != review.item_hash.0 {
        return Err(Error::validation(format!(
            "review targets item {} at hash {} but stored content hash is {}",
            review.item_id,
            review.item_hash.short(),
            &hash[..hash.len().min(12)]
        )));
    }

    sqlx::query(
        "INSERT INTO reviews (id, run_id, item_id, item_hash, strategy, body, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(review.id.as_str())
    .bind(review.run_id.as_str())
    .bind(review.item_id.as_str())
    .bind(review.item_hash.as_str())
    .bind(review.strategy.as_str())
    .bind(serde_json::to_string(review)?)
    .bind(to_rfc3339(review.created_at))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The §4.3 gate. Evidence kinds are resolved from storage rather than
/// trusted from the caller, and the basis must exist.
pub(crate) async fn record_assessment_in_tx(
    conn: &mut SqliteConnection,
    record: &AssessmentRecord,
) -> Result<()> {
    let mut kinds = Vec::with_capacity(record.evidence.len());
    for link in &record.evidence {
        let body: Option<String> = sqlx::query_scalar("SELECT body FROM evidence WHERE id = ?")
            .bind(link.evidence_id.as_str())
            .fetch_optional(&mut *conn)
            .await?;
        let Some(body) = body else {
            return Err(Error::not_found(format!(
                "assessment cites unknown evidence {}",
                link.evidence_id
            )));
        };
        let evidence: Evidence = serde_json::from_str(&body)?;
        kinds.push(evidence.kind);
    }

    validate_assessment(
        &record.item_id,
        record.assessment,
        &record.evidence,
        &kinds,
        record.basis_review.as_ref(),
        record.basis_decision.as_ref(),
    )?;

    // A basis that does not exist is no basis at all: §4.3 requires the
    // assessment to reference a Review or Decision *explaining* it.
    if let Some(review_id) = &record.basis_review {
        let found: Option<String> = sqlx::query_scalar("SELECT id FROM reviews WHERE id = ?")
            .bind(review_id.as_str())
            .fetch_optional(&mut *conn)
            .await?;
        if found.is_none() {
            return Err(Error::not_found(format!(
                "assessment cites review {review_id}, which does not exist"
            )));
        }
    }
    if let Some(decision_id) = &record.basis_decision {
        let found: Option<String> = sqlx::query_scalar("SELECT id FROM decisions WHERE id = ?")
            .bind(decision_id.as_str())
            .fetch_optional(&mut *conn)
            .await?;
        if found.is_none() {
            return Err(Error::not_found(format!(
                "assessment cites decision {decision_id}, which does not exist"
            )));
        }
    }

    sqlx::query(
        "INSERT INTO assessments (item_id, assessment, basis_review, basis_decision, body, assessed_at)
             VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(record.item_id.as_str())
    .bind(record.assessment.as_str())
    .bind(record.basis_review.as_ref().map(|r| r.0.clone()))
    .bind(record.basis_decision.as_ref().map(|d| d.0.clone()))
    .bind(serde_json::to_string(record)?)
    .bind(to_rfc3339(record.assessed_at))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(crate) async fn insert_decision_in_tx(
    conn: &mut SqliteConnection,
    decision: &Decision,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO decisions (id, run_id, kind, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(decision.id.as_str())
    .bind(decision.run_id.as_str())
    .bind(decision.kind.as_str())
    .bind(serde_json::to_string(decision)?)
    .bind(to_rfc3339(decision.created_at))
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn insert_artifact_in_tx(
    conn: &mut SqliteConnection,
    artifact: &Artifact,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO artifacts (id, run_id, content_hash, media_type, access,
                                    storage_path, body, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(artifact.id.as_str())
    .bind(artifact.run_id.as_str())
    .bind(artifact.content_hash.as_str())
    .bind(&artifact.media_type)
    .bind(artifact.access.as_str())
    .bind(&artifact.storage_path)
    .bind(serde_json::to_string(artifact)?)
    .bind(to_rfc3339(artifact.created_at))
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn insert_experiment_in_tx(
    conn: &mut SqliteConnection,
    exp: &Experiment,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO experiments (id, run_id, external, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(exp.id.as_str())
    .bind(exp.run_id.as_str())
    .bind(exp.external as i32)
    .bind(serde_json::to_string(exp)?)
    .bind(to_rfc3339(exp.created_at))
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn insert_cluster_in_tx(
    conn: &mut SqliteConnection,
    cluster: &Cluster,
) -> Result<()> {
    sqlx::query("INSERT INTO clusters (id, run_id, body, created_at) VALUES (?, ?, ?, ?)")
        .bind(cluster.id.as_str())
        .bind(cluster.run_id.as_str())
        .bind(serde_json::to_string(cluster)?)
        .bind(to_rfc3339(cluster.created_at))
        .execute(conn)
        .await?;
    Ok(())
}

pub(crate) async fn insert_feedback_in_tx(
    conn: &mut SqliteConnection,
    feedback: &Feedback,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO feedback (id, run_id, revision, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(feedback.id.as_str())
    .bind(feedback.run_id.as_str())
    .bind(feedback.revision)
    .bind(serde_json::to_string(feedback)?)
    .bind(to_rfc3339(feedback.created_at))
    .execute(conn)
    .await?;
    Ok(())
}
