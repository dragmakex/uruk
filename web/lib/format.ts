/**
 * Presentation helpers. Everything here renders real recorded facts;
 * nothing estimates or invents (no fake "est. 4 min").
 */

import type { RunState } from "./types";
import { TERMINAL_STATES } from "./types";

/** `mm:ss` under an hour, `h:mm:ss` from there, clamped at zero. */
export function formatClock(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(s / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const seconds = s % 60;
  const mm = String(minutes).padStart(2, "0");
  const ss = String(seconds).padStart(2, "0");
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}

/** Whole seconds between two RFC-3339 instants, or null if unparsable. */
export function elapsedBetween(from: string, to: string): number | null {
  const start = Date.parse(from);
  const end = Date.parse(to);
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  return Math.floor((end - start) / 1000);
}

/** The figure caption's compact run tag: the id's last four characters. */
export function shortRunId(id: string): string {
  return id.length > 4 ? id.slice(-4) : id;
}

/**
 * The ranking method the run was started with. Both methods keep Elo; the
 * recorded fact is the debate turn cap (1 = single-turn comparisons).
 */
export function rankingMethod(maxDebateTurns: number): "simple" | "tournament" {
  return maxDebateTurns > 1 ? "tournament" : "simple";
}

/**
 * Round header: `iterations` counts completed campaign rounds, so a live
 * run is in round `iterations + 1`, capped at the budget.
 */
export function roundLabel(
  state: RunState,
  iterations: number,
  maxIterations: number,
): string {
  if (isTerminal(state)) {
    return `${iterations} of ${maxIterations} rounds`;
  }
  const current = Math.min(iterations + 1, maxIterations);
  return `round ${current} of ${maxIterations}`;
}

export function isTerminal(state: RunState): boolean {
  return TERMINAL_STATES.includes(state);
}

/** Human text for a run state; always rendered as text, never color-only. */
export function stateLabel(state: RunState): string {
  switch (state) {
    case "waiting-for-human":
      return "waiting for you";
    case "budget-exhausted":
      return "budget exhausted";
    default:
      return state;
  }
}

/** Elo as the figure renders it: a whole number. */
export function formatElo(rating: number): string {
  return String(Math.round(rating));
}

/** Compact integer with thousands separators for token counts. */
export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}
