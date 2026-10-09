"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { ApiFailure, restartRun, resumeRun, stopRun } from "@/lib/api";
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

/**
 * Elapsed *active* seconds: live runs tick against now, paused and
 * finished runs freeze at their last recorded instant. Time spent paused
 * is subtracted, matching the wall-clock budget, which excludes it.
 */
function useElapsedSeconds(snapshot: RunSnapshot | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  const ticking =
    snapshot !== null &&
    !isTerminal(snapshot.run.state) &&
    snapshot.run.state !== "paused";

  useEffect(() => {
    if (!ticking) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);

  if (snapshot === null) return null;
  const pausedSecs = Math.floor(snapshot.run.paused_ms / 1000);
  if (!ticking) {
    // For a paused run `updated_at` is the pause instant.
    const frozen = elapsedBetween(snapshot.run.created_at, snapshot.run.updated_at);
    return frozen === null ? null : Math.max(0, frozen - pausedSecs);
  }
  const started = Date.parse(snapshot.run.created_at);
  if (Number.isNaN(started)) return null;
  return Math.max(0, Math.floor((now - started) / 1000) - pausedSecs);
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
  const router = useRouter();
  const stream = useRunStream(runId, initial);
  const snapshot = stream.snapshot;
  const elapsed = useElapsedSeconds(snapshot);
  const [acting, setActing] = useState<"pause" | "resume" | "restart" | null>(
    null,
  );
  const [actionError, setActionError] = useState<string | null>(null);

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

  async function act(
    kind: "pause" | "resume" | "restart",
    call: () => Promise<void>,
  ) {
    setActing(kind);
    setActionError(null);
    try {
      await call();
    } catch (e) {
      setActionError(
        e instanceof ApiFailure ? e.message : `${kind} request failed`,
      );
    } finally {
      setActing(null);
    }
  }

  async function onPause() {
    const sure = window.confirm(
      "Pause this run? Queued work waits, paused time does not count " +
        "against the wall-clock budget, and you can resume any time.",
    );
    if (!sure) return;
    // The stream reports the paused state; nothing else to do here.
    await act("pause", () => stopRun(runId));
  }

  async function onResume() {
    await act("resume", () => resumeRun(runId));
  }

  async function onRestart() {
    const sure = window.confirm(
      "Start a fresh run from this run's original configuration? " +
        "It begins with none of this run's results and spends its own budget.",
    );
    if (!sure) return;
    await act("restart", async () => {
      const freshId = await restartRun(runId);
      router.push(`/runs/${encodeURIComponent(freshId)}`);
    });
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
  const paused = run.state === "paused";
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
          {!terminal && !paused && (
            <button
              type="button"
              className="btn btn-outline"
              onClick={onPause}
              disabled={acting !== null}
            >
              {acting === "pause" ? "Pausing" : "Pause"}
            </button>
          )}
          {paused && (
            <button
              type="button"
              className="btn btn-outline"
              onClick={onResume}
              disabled={acting !== null}
            >
              {acting === "resume" ? "Resuming" : "Resume"}
            </button>
          )}
        </div>
      </div>

      <div className="canvas">
        <p className="visually-hidden" aria-live="polite">
          {announcement}
        </p>

        {actionError !== null && (
          <div className="error-box" role="alert" style={{ marginBottom: 24 }}>
            <h2>Request failed</h2>
            <p>{actionError}</p>
          </div>
        )}

        {paused && (
          <div className="empty-state" style={{ marginBottom: 32, maxWidth: "none" }}>
            <h2>Run paused</h2>
            <p>
              Queued work is waiting, and paused time does not count against
              the wall-clock budget. Resume to continue, or restart to begin
              a fresh run from the same configuration.
            </p>
            <button
              type="button"
              className="btn btn-outline"
              onClick={onRestart}
              disabled={acting !== null}
            >
              {acting === "restart" ? "Restarting" : "Restart"}
            </button>
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
            <button
              type="button"
              className="btn btn-outline"
              onClick={onRestart}
              disabled={acting !== null}
            >
              {acting === "restart" ? "Restarting" : "Restart"}
            </button>
          </div>
        )}

        {stats.pending_approvals > 0 && (
          <div className="error-box" role="status" style={{ marginBottom: 24 }}>
            <h2>Waiting for you</h2>
            <p>
              {stats.pending_approvals} approval request
              {stats.pending_approvals === 1 ? "" : "s"} pending. Decide with:
              uruk approve (or deny) on the command line.
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
