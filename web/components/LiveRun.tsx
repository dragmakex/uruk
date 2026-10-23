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

  // A provider recovery is worth saying only as a transition: the head
  // was waiting in an earlier snapshot and is not any more, while this
  // run still waits on the queue. A first snapshot that is simply not
  // waiting announces nothing, and leaving the queue clears the notice.
  // Tracked as "state derived from the previous snapshot", adjusted
  // during render, so no effect has to set state after the fact.
  const queueWaiting: boolean | null =
    snapshot !== null && snapshot.run.queue_position !== null
      ? snapshot.run.queue_provider_waiting
      : null;
  const [seenQueueWaiting, setSeenQueueWaiting] = useState(queueWaiting);
  const [providerRecovered, setProviderRecovered] = useState(false);
  if (queueWaiting !== seenQueueWaiting) {
    setSeenQueueWaiting(queueWaiting);
    setProviderRecovered(seenQueueWaiting === true && queueWaiting === false);
  }

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
  // A queue place, not the lifecycle state, says the run is waiting:
  // durable recovery can put a `running` run back on the queue, and it waits
  // like any other.
  const onQueue = run.queue_position !== null;
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

        {run.queue_position !== null && (
          <div
            className="empty-state"
            role="status"
            aria-label="Queued"
            style={{ marginBottom: 32, maxWidth: "none" }}
          >
            <h2>Queued, position {run.queue_position}</h2>
            <p>
              Runs execute one at a time: yours in the order you started
              them, taking turns with other researchers&apos; runs:{" "}
              {queueNote(run.queue_position)}. Research begins when this run
              reaches its turn. Stop remains available.
            </p>
          </div>
        )}

        {onQueue && run.queue_provider_waiting && (
          <div
            className="error-box"
            role="status"
            aria-label="Model provider status"
            style={{ marginBottom: 24 }}
          >
            <h2>The active run is waiting for the model provider</h2>
            <p>
              Your place in the queue is preserved and this run will start
              automatically when its turn comes; no action is required. Your
              run has not started.
            </p>
          </div>
        )}

        {onQueue && !run.queue_provider_waiting && providerRecovered && (
          <div
            className="empty-state"
            role="status"
            aria-label="Model provider status"
            style={{ marginBottom: 24, maxWidth: "none" }}
          >
            <h2>Model provider available again</h2>
            <p>
              The queue is moving again. Your place is unchanged and this run
              will start automatically when its turn comes; no action is
              required.
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
              no action is required. Stop remains available.
            </p>
          </div>
        )}

        {/* A run waiting on the queue has no agents, matches, or
            candidates to show: an empty figure would only suggest activity
            that is not there. */}
        {!onQueue && (
          <>
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
          </>
        )}
      </div>
    </>
  );
}
