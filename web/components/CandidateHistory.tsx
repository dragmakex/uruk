"use client";

import { useEffect, useState } from "react";
import { fetchCandidateHistory } from "@/lib/api";
import { formatDelta, formatElo, formatModelIdentity } from "@/lib/format";
import type {
  CandidateHistory as History,
  HistoryEntry,
  HistoryResult,
  MatchMethod,
} from "@/lib/types";

const RESULT_WORD: Record<HistoryResult, string> = {
  won: "won",
  lost: "lost",
  drew: "drew",
  unresolved: "no basis to decide",
};

/** The one-letter mark, so a column of results scans vertically. */
const RESULT_MARK: Record<HistoryResult, string> = {
  won: "W",
  lost: "L",
  drew: "=",
  unresolved: "–",
};

const METHOD_LABEL: Record<MatchMethod, string> = {
  pairwise: "one-turn comparison",
  debate: "multi-turn debate",
};

/** "2 won, 1 lost" — only the results that actually occurred. */
function tally(matches: readonly HistoryEntry[]): string {
  const counts = new Map<HistoryResult, number>();
  for (const match of matches) {
    counts.set(match.result, (counts.get(match.result) ?? 0) + 1);
  }
  const order: HistoryResult[] = ["won", "lost", "drew", "unresolved"];
  return order
    .filter((result) => (counts.get(result) ?? 0) > 0)
    .map((result) => `${counts.get(result)} ${RESULT_WORD[result]}`)
    .join(", ");
}

function HistoryRow({ entry }: { entry: HistoryEntry }) {
  return (
    <li className={`history-row is-${entry.result}`}>
      <span className="history-mark" aria-hidden="true">
        {RESULT_MARK[entry.result]}
      </span>
      <span className="visually-hidden">{RESULT_WORD[entry.result]} </span>
      <span className="history-opponent">{entry.opponent_title}</span>
      <span className="history-elo micro-ink">
        {formatElo(entry.rating_before)}
        <span aria-hidden="true"> {"\u2192"} </span>
        <span className="visually-hidden"> became </span>
        {formatElo(entry.rating_after)}
      </span>
      <span
        className={`history-delta micro ${
          entry.delta > 0 ? "delta-up" : entry.delta < 0 ? "delta-down" : ""
        }`}
      >
        {formatDelta(entry.delta)}
      </span>
      <span className="history-method micro">
        {METHOD_LABEL[entry.method]} · judge {formatModelIdentity(entry.judge_model)}
      </span>
      <p className="history-rationale">{entry.rationale}</p>
    </li>
  );
}

/**
 * One candidate's matches, read from the record the first time the panel is
 * opened. The read is a second pass over facts the engine already wrote: a
 * failure leaves the panel saying so rather than showing an empty history,
 * and a candidate with no matches says exactly that.
 */
export function CandidateHistory({
  runId,
  itemId,
}: {
  runId: string;
  itemId: string;
}) {
  // One state, so a read that fails says so and a read in flight says
  // nothing else. Three separate booleans could disagree with each other.
  const [read, setRead] = useState<
    { status: "reading" } | { status: "ready"; history: History } | { status: "failed" }
  >({ status: "reading" });

  useEffect(() => {
    let live = true;
    fetchCandidateHistory(runId, itemId)
      .then((next) => {
        if (live) setRead({ status: "ready", history: next });
      })
      .catch(() => {
        if (live) setRead({ status: "failed" });
      });
    return () => {
      live = false;
    };
  }, [runId, itemId]);

  if (read.status === "failed") {
    return <p className="history-note micro">history could not be read</p>;
  }
  if (read.status === "reading") {
    return (
      <p className="history-note micro" role="status">
        reading history…
      </p>
    );
  }
  const history = read.history;
  if (history.matches.length === 0) {
    return <p className="history-note micro">no recorded matches yet</p>;
  }

  return (
    <div className="history">
      <p className="history-tally micro-ink">
        {history.matches.length} match
        {history.matches.length === 1 ? "" : "es"} · {tally(history.matches)} ·
        newest first
      </p>
      <ol className="history-rows">
        {history.matches.map((entry) => (
          <HistoryRow key={entry.id} entry={entry} />
        ))}
      </ol>
    </div>
  );
}
