"use client";

import { useEffect, useState } from "react";
import { fetchDecisions } from "@/lib/api";
import type { DecisionLedger as Ledger, DecisionView } from "@/lib/types";

/**
 * What each recorded kind of decision is, in words a reader can use. The
 * label is presentation only: the record's own `kind` string is kept for
 * anything this build does not recognise, so an unknown kind shows up as
 * itself instead of vanishing from the ledger.
 */
const KIND_LABEL: Record<string, string> = {
  plan_change: "plan changed",
  human_feedback: "researcher weighed in",
  scheduling: "work scheduled",
  disposition: "candidate disposition",
  meta_feedback: "metacognitive feedback",
  stop: "run stopped",
};

function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind.replaceAll("_", " ");
}

function DecisionRow({ decision, index }: { decision: DecisionView; index: number }) {
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
 * Re-read whenever the snapshot's decision count moves, so a live run's
 * ledger fills in as it decides. A failed read says so; it never renders as
 * "no decisions".
 */
export function DecisionLedger({
  runId,
  decisionCount,
}: {
  runId: string;
  /** `stats.decisions` from the snapshot: the re-read trigger. */
  decisionCount: number;
}) {
  const [read, setRead] = useState<
    { status: "reading" } | { status: "ready"; ledger: Ledger } | { status: "failed" }
  >({ status: "reading" });

  useEffect(() => {
    let live = true;
    fetchDecisions(runId)
      .then((next) => {
        if (live) setRead({ status: "ready", ledger: next });
      })
      .catch(() => {
        if (live) setRead({ status: "failed" });
      });
    return () => {
      live = false;
    };
  }, [runId, decisionCount]);

  if (read.status === "failed") {
    return <p className="micro">decisions could not be read</p>;
  }
  if (read.status === "reading") {
    return (
      <p className="micro" role="status">
        reading decisions…
      </p>
    );
  }
  if (read.ledger.decisions.length === 0) {
    return (
      <p className="micro">
        No decisions on record yet. The supervisor records one whenever it
        changes the plan, schedules work, or stops the run.
      </p>
    );
  }

  return (
    <ol className="decision-rows" aria-label="Decisions, oldest first">
      {read.ledger.decisions.map((decision, index) => (
        <DecisionRow key={decision.id} decision={decision} index={index} />
      ))}
    </ol>
  );
}
