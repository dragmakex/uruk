"use client";

import { useRouter } from "next/navigation";
import { useId, useState } from "react";
import { ApiFailure, startRun } from "@/lib/api";

const MAX_GOAL_CHARS = 8000;

/**
 * Real Uruk limits sent with the request, plus the selected ranking depth.
 * Google's AI co-scientist paper calls tournament ranking computationally
 * intensive but does not publish fixed per-mode wall-clock durations, so this
 * deliberately states limits rather than an estimated completion time.
 */
export const TOURNAMENT_BUDGET_NOTE =
  "Tournament · debates up to 5 turns · limits: 200 model calls, 60 min, 10 rounds";

/**
 * The run specification form: research goal plus the tournament method.
 * Submits `POST /api/runs` and navigates to the live run.
 *
 * Web runs use the paper-style ranking path: pairwise multi-turn scientific
 * debate, Elo ranking, and iterative refinement. The API stores a five-turn
 * debate cap on the run's budget.
 */
export function RunForm() {
  const router = useRouter();
  const goalId = useId();
  const [goal, setGoal] = useState("");

  const [submitting, setSubmitting] = useState(false);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [apiError, setApiError] = useState<{ error: string; kind: string } | null>(
    null,
  );

  async function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setApiError(null);

    const question = goal.trim();
    if (question.length === 0) {
      setFieldError("State the research goal first.");
      return;
    }
    if (question.length > MAX_GOAL_CHARS) {
      setFieldError(`The goal is limited to ${MAX_GOAL_CHARS} characters.`);
      return;
    }
    setFieldError(null);
    setSubmitting(true);

    try {
      const runId = await startRun({
        goal: question,
        mode: "campaign",
        ranking: "tournament",
      });
      router.push(`/runs/${encodeURIComponent(runId)}`);
    } catch (e) {
      if (e instanceof ApiFailure) {
        setApiError({ error: e.message, kind: e.kind });
      } else {
        setApiError({ error: "unexpected client error", kind: "client" });
      }
      setSubmitting(false);
    }
  }

  return (
    <form className="panel" onSubmit={onSubmit} noValidate>
      <div className="panel-head">
        <h1 className="panel-title">Run specification</h1>
        <span className="micro">new</span>
      </div>

      <div className="panel-section">
        <label className="section-label" htmlFor={goalId}>
          Research goal
        </label>
        <textarea
          id={goalId}
          className="goal-input"
          name="goal"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
          placeholder="Question, system, constraints, what would count as an answer."
          maxLength={MAX_GOAL_CHARS + 1}
          aria-invalid={fieldError !== null}
          aria-describedby={fieldError ? `${goalId}-error` : undefined}
        />
        {fieldError !== null && (
          <p className="field-error" id={`${goalId}-error`} role="alert">
            {fieldError}
          </p>
        )}
      </div>

      <div className="panel-section">
        <span className="section-label">Method</span>
        <div className="ranking-summary">
          <strong>Tournament ranking</strong>
          <span>
            Multi-turn scientific debates, Elo ranking, and iterative refinement.
            Runtime varies by research goal.
          </span>
        </div>
      </div>

      <div className="panel-foot">
        <span className="micro" aria-live="polite">
          {TOURNAMENT_BUDGET_NOTE}
        </span>
        <button type="submit" className="btn" disabled={submitting}>
          {submitting ? "Starting run" : "Start research"}
        </button>
      </div>

      {apiError !== null && (
        <div className="panel-section" role="alert">
          <p className="field-error">
            {apiError.kind === "unreachable"
              ? "The uruk API is unreachable. Start it with: uruk serve"
              : apiError.error}
          </p>
          <p className="error-kind">kind: {apiError.kind}</p>
        </div>
      )}
    </form>
  );
}
