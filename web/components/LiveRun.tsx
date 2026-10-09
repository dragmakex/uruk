"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, stopRun } from "@/lib/api";
import {
  elapsedBetween,
  formatClock,
  isTerminal,
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

/** Elapsed run seconds: live runs tick against now, finished runs freeze. */
function useElapsedSeconds(snapshot: RunSnapshot | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  const running = snapshot !== null && !isTerminal(snapshot.run.state);

  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [running]);

  if (snapshot === null) return null;
  if (!running) {
    return elapsedBetween(snapshot.run.created_at, snapshot.run.updated_at);
  }
  const started = Date.parse(snapshot.run.created_at);
  if (Number.isNaN(started)) return null;
  return Math.max(0, Math.floor((now - started) / 1000));
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
            <button
              type="button"
              className="btn btn-outline"
              onClick={onStop}
              disabled={stopping}
            >
              {stopping ? "Stopping" : "Stop"}
            </button>
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
            <Link href={`/runs/${encodeURIComponent(run.id)}/report`} className="btn">
              Read the report
            </Link>{" "}
            <Link
              href={`/runs/${encodeURIComponent(run.id)}/citations`}
              className="btn btn-outline"
            >
              Inspect citations
            </Link>
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
