"use client";

import { Button, ButtonGroup } from "./Button";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, stopRun } from "@/lib/api";
import {
  elapsedBetween,
  formatClock,
  isTerminal,
  providerWaitNote,
  queueNote,
  rankingMethod,
  roundLabel,
  shortRunId,
  stateLabel,
} from "@/lib/format";
import { useRunStream, type StreamStatus } from "@/lib/sse";
import type { RunSnapshot } from "@/lib/types";
import { RankingList } from "./RankingList";
import { Topology } from "./Topology";

function connectionNote(status: StreamStatus): string | null {
  switch (status) {
    case "connecting":
      return "connecting";
    case "reconnecting":
      return "connection lost, retrying";
    case "failed":
      return "stream closed, data may be stale";
    case "ended":
      return null; // The terminal banner already says the run finished.
    case "live":
      return null;
  }
}

/**
 * Research seconds consumed: from the run's first start, minus every
 * settled provider wait and the one still open. Null while queued. Live
 * runs tick against now (frozen during a provider wait), finished runs
 * freeze at their last update.
 */
function useElapsedSeconds(snapshot: RunSnapshot | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  const running = snapshot !== null && !isTerminal(snapshot.run.state);

  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [running]);

  if (snapshot === null || snapshot.run.started_at === null) return null;
  const settled = snapshot.run.paused_secs;
  if (!running) {
    const total = elapsedBetween(snapshot.run.started_at, snapshot.run.updated_at);
    return total === null ? null : Math.max(0, total - settled);
  }
  const wait = snapshot.run.provider_wait;
  const end = wait !== null ? Date.parse(wait.since) : now;
  const started = Date.parse(snapshot.run.started_at);
  if (Number.isNaN(started) || Number.isNaN(end)) return null;
  return Math.max(0, Math.floor((end - started) / 1000) - settled);
}

/**
 * The live run view: sticky header with real budget and state facts, the
 * connected agent topology, the line-style legend, and the ranking cards.
 * State arrives as complete snapshots over SSE; this component renders the
 * latest one and never extrapolates.
 */
export function LiveRun({
  runId,
  initial,
}: {
  runId: string;
  initial: RunSnapshot | null;
}) {
  const stream = useRunStream(runId, initial);
  const snapshot = stream.snapshot;
  const elapsed = useElapsedSeconds(snapshot);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);

  // Announce state transitions politely, not every snapshot.
  const [announcement, setAnnouncement] = useState("");
  const lastAnnounced = useRef<string | null>(null);
  useEffect(() => {
    if (snapshot === null) return;
    const key = snapshot.run.state;
    if (lastAnnounced.current !== key) {
      lastAnnounced.current = key;
      setAnnouncement(`Run is ${stateLabel(snapshot.run.state)}.`);
    }
  }, [snapshot]);

  async function onStop() {
    if (!window.confirm("Stop this run? Pending work is cancelled.")) return;
    setStopping(true);
    setStopError(null);
    try {
      await stopRun(runId);
      // The stream reports the cancelled state; nothing else to do here.
    } catch (e) {
      setStopError(e instanceof ApiFailure ? e.message : "stop request failed");
    } finally {
      setStopping(false);
    }
  }

  if (snapshot === null) {
    return (
      <div className="canvas">
        {stream.status === "failed" ? (
          <div className="error-box" role="alert">
            <h1>The run stream is unavailable</h1>
            <p>
              The uruk API did not answer for run {runId}. Check that
              `uruk serve` is running, then reload.
            </p>
            <p className="error-kind">status: stream closed</p>
          </div>
        ) : stream.parseErrors > 0 ? (
          // Frames arrive but fail validation: a frontend/API contract
          // drift. Say so rather than showing "Connecting" forever.
          <div className="error-box" role="alert">
            <h1>The run snapshot could not be read</h1>
            <p>
              The API is answering, but its snapshots do not match what this
              console expects ({stream.parseErrors} rejected). The frontend
              and `uruk serve` are probably different versions; rebuild or
              restart both.
            </p>
            <p className="error-kind">status: contract drift</p>
          </div>
        ) : (
          <div className="empty-state" aria-busy="true">
            <h1>Connecting</h1>
            <p>Waiting for the first run snapshot from the API.</p>
          </div>
        )}
      </div>
    );
  }

  const { run, goal, stats, agents, items, usage } = snapshot;
  const terminal = isTerminal(run.state);
  const queued = run.state === "queued";
  const note = connectionNote(stream.status);
  const method = rankingMethod(goal.budget.max_debate_turns);

  return (
    <>
      <div className="run-head">
        <h1 className="run-head-question" title={goal.question}>
          {goal.question}
        </h1>
        <div className="run-head-facts">
          <span className="micro-ink">{method}</span>
          <span className="micro-ink">
            {roundLabel(run.state, run.iterations, goal.budget.max_iterations)}
          </span>
          <span className="micro-ink" aria-label="elapsed time">
            {elapsed !== null ? formatClock(elapsed) : "--:--"}
          </span>
          {note !== null && <span className="conn-note">{note}</span>}
          {!terminal && (
            <Button
              variant="outline" size="sm"
              onClick={onStop}
              disabled={stopping}
            >
              {stopping ? "Stopping" : "Stop"}
            </Button>
          )}
        </div>
      </div>

      <div className="canvas">
        <p className="visually-hidden" aria-live="polite">
          {announcement}
        </p>

        {stopError !== null && (
          <div className="error-box" role="alert" style={{ marginBottom: 24 }}>
            <h2>Stop failed</h2>
            <p>{stopError}</p>
          </div>
        )}

        {terminal && (
          <div className="empty-state" style={{ marginBottom: 32, maxWidth: "none" }}>
            <h2>
              Run {stateLabel(run.state)}
              {run.stop_condition ? ` (${run.stop_condition.replace(/_/g, " ")})` : ""}
            </h2>
            <p>
              {usage.model_calls} model calls, {usage.tokens.toLocaleString("en-US")}{" "}
              tokens,{" "}
              {usage.cost_usd !== null
                ? `$${usage.cost_usd.toFixed(4)}`
                : "cost unknown"}
              .
            </p>
            <ButtonGroup equal>
              <Button
                href={`/runs/${encodeURIComponent(run.id)}/report`}
                variant="primary"
                size="cta"
              >
                Read the report
              </Button>
              <Button
                href={`/runs/${encodeURIComponent(run.id)}/citations`}
                variant="outline"
                size="cta"
              >
                Inspect citations
              </Button>
            </ButtonGroup>
          </div>
        )}

        {queued && (
          <div
            className="empty-state"
            role="status"
            aria-label="Queued"
            style={{ marginBottom: 32, maxWidth: "none" }}
          >
            <h2>Queued, position {run.queue_position ?? "unknown"}</h2>
            <p>
              Runs execute one at a time in the order they were started:{" "}
              {run.queue_position !== null
                ? queueNote(run.queue_position)
                : "waiting for the runs ahead"}
              . Research begins, and the time budget starts, when this run
              reaches the head. Stop remains available.
            </p>
          </div>
        )}

        {!terminal && run.provider_wait !== null && (
          <div
            className="error-box"
            role="status"
            aria-label="Waiting for the model provider"
            style={{ marginBottom: 24 }}
          >
            <h2>Waiting for the model provider</h2>
            <p>
              This run is still live: it is {providerWaitNote(run.provider_wait)}.
              Work resumes automatically when the provider answers again, and
              this wait does not count against the run&apos;s time budget.
              Stop remains available.
            </p>
          </div>
        )}

        {stats.pending_approvals > 0 && (
          <div className="error-box" role="status" style={{ marginBottom: 24 }}>
            <h2>Waiting for you</h2>
            <p>
              {stats.pending_approvals} approval request
              {stats.pending_approvals === 1 ? "" : "s"} pending. Approvals
              cannot be decided in the web console yet: an approval binds to
              the exact payload being granted, and this console has no UI
              that shows that payload. The run stays paused; computational
              execution is unavailable until that approval UI exists.
            </p>
          </div>
        )}

        <div className="fig-row">
          <span className="micro">
            Fig. run {shortRunId(run.id)}, {roundLabel(run.state, run.iterations, goal.budget.max_iterations)}
          </span>
          <span className="micro">
            {stats.items} items, {stats.matches} matches
            {stats.leader
              ? `, leader elo ${Math.round(stats.leader.rating)}`
              : ""}
          </span>
        </div>

        <Topology agents={agents} stats={stats} />

        <details className="legend-fold" open>
          <summary>Key</summary>
          <div className="legend-line">
            <span className="micro">Filled header: thinking</span>
            <span className="micro">Dashed wire: carrying work now</span>
            <span className="micro">Dashed box: idle</span>
          </div>
        </details>

        <section className="items-section" aria-label="Ranked candidates">
          <div className="items-head">
            <h2>Candidates</h2>
            <span className="micro">
              {usage.model_calls} of {goal.budget.max_model_calls} model calls
              used
            </span>
          </div>
          <RankingList items={items} />
        </section>
      </div>
    </>
  );
}
