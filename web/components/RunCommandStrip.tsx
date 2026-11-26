import type { RunSnapshot } from "@/lib/types";
import { stateLabel } from "@/lib/format";
import "./run-command-strip.css";

/** Persisted run facts, not estimates of progress, provider cost or activity. */
export function RunCommandStrip({ snapshot }: { snapshot: RunSnapshot }) {
  const { run, goal, stats, usage } = snapshot;
  const total = goal.budget.max_model_calls;
  const fraction = total > 0 ? Math.min(1, usage.model_calls / total) : 0;
  const exhausted = run.state === "budget-exhausted";
  const status = run.queue_position !== null
    ? `Queued #${run.queue_position}`
    : run.provider_wait !== null && run.state === "running"
      ? "Waiting for provider"
      : stateLabel(run.state).replace(/^./, (letter) => letter.toUpperCase());
  const cost = usage.cost_usd === null
    ? "Cost unknown"
    : `$${usage.cost_usd.toFixed(4)}${usage.cost_partially_unknown ? " + unknown" : ""}`;

  return (
    <div className="run-command-strip" aria-label="Run command strip">
      <div className="run-command-facts micro-ink">
        <span>{status}</span>
        <span>{usage.model_calls.toLocaleString("en-US")} / {total.toLocaleString("en-US")} calls</span>
        <span>{stats.items.toLocaleString("en-US")} items</span>
        <span>{stats.matches.toLocaleString("en-US")} matches</span>
        <span title={stats.leader?.title}>
          {stats.leader === null ? "Leader unrated" : `Leader Elo ${Math.round(stats.leader.rating).toLocaleString("en-US")}`}
        </span>
        <span>{cost}</span>
      </div>
      <div
        className="run-budget-bar"
        role="progressbar"
        aria-label="Model-call budget"
        aria-valuemin={0}
        aria-valuemax={Math.max(1, total)}
        aria-valuenow={Math.min(usage.model_calls, total)}
        aria-valuetext={`${usage.model_calls} of ${total} model calls used${exhausted ? "; budget exhausted" : ""}`}
        data-exhausted={exhausted}
      >
        <span style={{ transform: `scaleX(${fraction})` }} />
      </div>
    </div>
  );
}
