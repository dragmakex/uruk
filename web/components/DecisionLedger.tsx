"use client";

import { fetchDecisions } from "@/lib/api";
import type { DecisionView } from "@/lib/types";
import { LazyRecordPanel } from "./LazyRecordPanel";

/**
 * What each recorded kind of decision is, in words a reader can use. The
 * label is presentation only: the record's own `kind` string is kept for
 * anything this build does not recognise, so an unknown kind shows up as
 * itself instead of vanishing from the ledger.
 */
const KIND_LABEL: Record<string, string> = {
  scheduling: "work scheduled",
  disposition: "candidate disposition",
  meta_feedback: "metacognitive feedback",
  stop: "run stopped",
};

function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind.replaceAll("_", " ");
}

function DecisionRow({
  decision,
  index,
}: {
  decision: DecisionView;
  index: number;
}) {
  return (
    <li className={`decision-row kind-${decision.kind}`}>
      <div className="decision-head">
        <span className="micro-ink decision-index">
          D{String(index + 1).padStart(2, "0")}
        </span>
        <span className="decision-actor">{decision.actor}</span>
        <span className="micro decision-kind">{kindLabel(decision.kind)}</span>
      </div>
      <p className="decision-reason">{decision.reason}</p>
      {decision.referenced.length > 0 && (
        <p className="decision-refs micro">
          refers to {decision.referenced.join(", ")}
        </p>
      )}
    </li>
  );
}

/**
 * The run's decisions in the order they were taken: who chose, what they
 * chose, and the reason they gave. This is the part of the page that shows
 * the run reasoning for itself rather than reporting totals about it.
 *
 * Pages load on explicit actions only; live decisions mark the saved page
 * stale. A failed read says so; it never renders as "no decisions".
 */
export function DecisionLedger({
  runId,
  decisionCount,
}: {
  runId: string;
  /** `stats.decisions` from the snapshot: count and stale-page marker. */
  decisionCount: number;
}) {
  return (
    <LazyRecordPanel
      label="decisions"
      pagerLabel="Decisions"
      controls="decision-table"
      summary={`${decisionCount} ${decisionCount === 1 ? "decision" : "decisions"} recorded`}
      revision={String(decisionCount)}
      fetchPage={(page, signal) => fetchDecisions(runId, page, signal)}
    >
      {(ledger) =>
        ledger.decisions.length === 0 ? (
          <p className="micro">
            No decisions on record yet. The supervisor records one whenever it
            changes the plan, schedules work, or stops the run.
          </p>
        ) : (
          <ol
            id="decision-list"
            className="decision-rows record-table-body"
            aria-label="Decisions, oldest first"
          >
            {ledger.decisions.map((decision, index) => (
              <DecisionRow
                key={decision.id}
                decision={decision}
                index={ledger.offset + index}
              />
            ))}
          </ol>
        )
      }
    </LazyRecordPanel>
  );
}
