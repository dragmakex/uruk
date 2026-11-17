"use client";

import { useEffect, useState } from "react";
import { fetchTournament } from "@/lib/api";
import { formatDelta, formatElo, formatModelIdentity } from "@/lib/format";
import {
  RECORD_PAGE_SIZE,
  RecordTableControls,
  RecordTablePager,
} from "./RecordTableControls";
import type {
  MatchMethod,
  MatchOutcome,
  TournamentFeed as Feed,
  TournamentMatch,
  TournamentSide,
} from "@/lib/types";

const OUTCOME_LABEL: Record<MatchOutcome, string> = {
  win_a: "first side won",
  win_b: "second side won",
  draw: "draw",
  insufficient_basis: "no basis to decide",
};

const METHOD_LABEL: Record<MatchMethod, string> = {
  pairwise: "one-turn comparison",
  debate: "multi-turn debate",
};

/**
 * The recorded match log, refetched whenever the snapshot's match count
 * changes. The stream carries facts, not judgements: the log is a second
 * read, so a failed read leaves the log absent rather than invented.
 */
function useMatchLog(runId: string, knownMatches: number) {
  const [feed, setFeed] = useState<Feed | null>(null);
  const [unavailable, setUnavailable] = useState(false);

  useEffect(() => {
    let live = true;
    fetchTournament(runId)
      .then((next) => {
        if (!live) return;
        setFeed(next);
        setUnavailable(false);
      })
      .catch(() => {
        if (live) setUnavailable(true);
      });
    return () => {
      live = false;
    };
  }, [runId, knownMatches]);

  return { feed, unavailable };
}

/** Which side the verdict favoured, for marking the row. */
function verdictSide(outcome: MatchOutcome): "first" | "second" | null {
  if (outcome === "win_a") return "first";
  if (outcome === "win_b") return "second";
  return null;
}

function SideRow({
  side,
  mark,
  position,
}: {
  side: TournamentSide;
  mark: "won" | "lost" | "level";
  position: "first" | "second";
}) {
  return (
    <li className={`match-side match-side-${mark}`} data-position={position}>
      <span className="match-side-mark" aria-hidden="true">
        {mark === "won" ? "W" : mark === "lost" ? "L" : "="}
      </span>
      <span className="match-side-title">{side.title}</span>
      <span className="match-side-elo micro-ink">
        {formatElo(side.rating_before)}
        <span aria-hidden="true"> {"\u2192"} </span>
        <span className="visually-hidden"> became </span>
        {formatElo(side.rating_after)}
      </span>
      <span
        className={`match-side-delta micro ${mark === "won" ? "delta-up" : mark === "lost" ? "delta-down" : ""}`}
      >
        {formatDelta(side.delta)}
      </span>
    </li>
  );
}

/**
 * One match, as a scoreboard line: who faced whom in presentation order,
 * what the judge decided and why, and what it did to both ratings. Side
 * marks follow the verdict, so `W` always sits on the candidate that won
 * even though the API's `first`/`second` are presentation positions.
 */
function MatchCard({
  match,
  index,
  fresh,
}: {
  match: TournamentMatch;
  index: number;
  fresh: boolean;
}) {
  const winner = verdictSide(match.outcome);
  const markFor = (position: "first" | "second"): "won" | "lost" | "level" =>
    winner === null ? "level" : winner === position ? "won" : "lost";

  return (
    <li className="match-card" data-fresh={fresh ? "true" : undefined}>
      <div className="match-head">
        <span className="micro-ink">M{String(index).padStart(2, "0")}</span>
        <span className="micro">{METHOD_LABEL[match.method]}</span>
        <span className="micro">judge {formatModelIdentity(match.judge_model)}</span>
        {match.order_randomized && (
          <span className="micro" title="The pair was shuffled before judging.">
            order hidden
          </span>
        )}
        <span className="micro match-verdict">{OUTCOME_LABEL[match.outcome]}</span>
      </div>

      <ol className="match-sides" aria-label="Sides">
        <SideRow side={match.first} mark={markFor("first")} position="first" />
        <SideRow side={match.second} mark={markFor("second")} position="second" />
      </ol>

      <p className="match-rationale">{match.rationale}</p>

      <div className="match-foot">
        <span className="micro">
          {match.outcome === "insufficient_basis"
            ? "ratings unchanged: the judge declined to rank them"
            : "ratings settled on this verdict"}
        </span>
        {match.produced_by !== null && (
          <span className="micro">played by {match.produced_by}</span>
        )}
      </div>
    </li>
  );
}

/**
 * The tournament as it has been played: every match, newest first, with
 * the judge's own rationale. This is the bracket's ground truth, so it
 * shows only recorded matches: no projected pairings, no hypothetical
 * results, and nothing at all until the first verdict exists.
 */
export function TournamentFeed({
  runId,
  matchCount,
}: {
  runId: string;
  matchCount: number;
}) {
  const { feed, unavailable } = useMatchLog(runId, matchCount);
  const [open, setOpen] = useState(false);
  const [requestedPage, setRequestedPage] = useState(1);
  const matches = feed?.matches ?? [];
  const ordered = [...matches].reverse();
  const totalPages = Math.max(1, Math.ceil(ordered.length / RECORD_PAGE_SIZE));
  const page = Math.min(requestedPage, totalPages);
  const pageStart = (page - 1) * RECORD_PAGE_SIZE;
  const visibleMatches = ordered.slice(pageStart, pageStart + RECORD_PAGE_SIZE);

  if (matches.length === 0) {
    if (unavailable) {
      return (
        <p className="micro">
          The match log could not be read. Matches are still being recorded;
          this panel will fill in when the API answers.
        </p>
      );
    }
    if (feed === null) {
      return (
        <p className="micro" aria-busy="true">
          Reading the match log.
        </p>
      );
    }
    return (
      <p className="micro">
        No matches yet. The ranking agent plays comparisons once two
        candidates are ready to face each other.
      </p>
    );
  }

  // Newest first: the last match played is the one worth watching, and
  // revision order (not the match id) is the honest order.
  return (
    <div className="record-table-shell">
      <RecordTableControls
        label="match log"
        summary={`${matches.length} ${matches.length === 1 ? "match" : "matches"} recorded`}
        open={open}
        controls="match-log-table"
        onToggle={() => setOpen((visible) => !visible)}
      />

      <div id="match-log-table" hidden={!open}>
        <ol id="match-log-list" className="match-log" aria-label="Match log">
          {visibleMatches.map((match, index) => (
            <MatchCard
              key={match.id}
              match={match}
              index={matches.length - pageStart - index}
              fresh={page === 1 && index === 0 && matchCount > 0}
            />
          ))}
        </ol>

        <RecordTablePager
          label="Match log"
          page={page}
          totalPages={totalPages}
          onPage={setRequestedPage}
        />
      </div>
    </div>
  );
}
