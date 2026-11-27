/**
 * Observed task starts, drawn on supervisor-to-worker connections.
 * Coincident starts and finishes do not establish a transfer between workers;
 * snapshots do not carry the dependency evidence needed for that claim.
 */
import type { AgentPanel } from "./types";

export interface WorkMove {
  from: "supervisor";
  to: string;
  kind: "arrival";
}

/** New work on a previously inactive role; continuing work animates nothing. */
export function detectWorkMoves(
  previous: readonly AgentPanel[],
  next: readonly AgentPanel[],
): WorkMove[] {
  const before = new Map(
    previous.map((panel) => [panel.role, panel.current?.id ?? null]),
  );
  return next
    .filter(
      (panel) =>
        panel.role !== "supervisor" &&
        panel.current !== null &&
        !before.get(panel.role),
    )
    .map((panel) => ({ from: "supervisor", to: panel.role, kind: "arrival" }));
}

export function moveSentence(
  move: WorkMove,
  label: (role: string) => string,
): string {
  return `${label(move.to)} picked up new work.`;
}
