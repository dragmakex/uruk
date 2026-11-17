/**
 * Work moving through the pipeline, read off the snapshot stream.
 *
 * Nothing here is guessed from pacing or imagined for effect. A move exists
 * because a panel's `current` task changed between two snapshots:
 *
 * - `handoff` — the upstream role let go of its task at the same time, so
 *   both ends of the transfer were observed.
 * - `arrival` — new work started somewhere and no upstream release was seen
 *   nearby, so the wire it travels is named from the pipeline's static
 *   wiring rather than from an observed departure.
 *
 * The distinction is kept because the two claims are different, and the
 * animation is allowed to show only what the states support.
 */

import { ROLES, WIRES } from "./roles";
import type { AgentPanel } from "./types";

/** How long a released task still counts as the upstream side of a handoff. */
export const HANDOFF_WINDOW_MS = 20_000;

export type WorkMoveKind = "handoff" | "arrival";

export interface WorkMove {
  /** Role the work came from. */
  from: string;
  /** Role that picked it up. */
  to: string;
  kind: WorkMoveKind;
}

/** Releases remembered between snapshots, so a gap in sampling is survivable. */
export interface HandoffWatch {
  /** Role → snapshot clock (ms) at which it let go of a task. */
  stopped: Record<string, number>;
}

export const emptyWatch = (): HandoffWatch => ({ stopped: {} });

function currentTask(panel: AgentPanel | undefined): string | null {
  return panel?.current?.id ?? null;
}

function byRole(panels: readonly AgentPanel[]): Map<string, AgentPanel> {
  return new Map(panels.map((panel) => [panel.role, panel]));
}

/**
 * The role that feeds `role` according to the pipeline, restricted to roles
 * the topology actually draws a wire from.
 */
function upstreamOf(role: string): string | null {
  for (const meta of Object.values(ROLES)) {
    if (meta.feeds === role) {
      const wired = WIRES.some(([from, to]) => from === meta.id && to === role);
      if (wired) return meta.id;
    }
  }
  return WIRES.find(([, to]) => to === role)?.[0] ?? null;
}

/**
 * Compare two snapshots and report the work that moved between them.
 *
 * `watch` carries releases forward, so a task that finished in one snapshot
 * still counts as the upstream side of an arrival in the next one. A release
 * is consumed by the move it explains, so one departure animates once.
 */
export function detectWorkMoves(
  previous: readonly AgentPanel[],
  next: readonly AgentPanel[],
  watch: HandoffWatch,
  now: number,
  windowMs: number = HANDOFF_WINDOW_MS,
): { moves: WorkMove[]; watch: HandoffWatch } {
  const before = byRole(previous);
  const after = byRole(next);

  const stopped: Record<string, number> = {};
  for (const [role, at] of Object.entries(watch.stopped)) {
    if (now - at <= windowMs) stopped[role] = at;
  }

  // A role released its task when it had one and now has a different one,
  // or none. Same task, or no task before: nothing was let go.
  for (const [role, panel] of after) {
    const was = currentTask(before.get(role));
    if (was !== null && currentTask(panel) !== was) stopped[role] = now;
  }

  const moves: WorkMove[] = [];
  for (const [role, panel] of after) {
    const task = currentTask(panel);
    if (task === null || task === currentTask(before.get(role))) continue;

    // A panel that swapped its running task for its own next one carried
    // on working; no wire brought it anything new.
    if (stopped[role] !== undefined) continue;

    const upstream = upstreamOf(role);
    if (upstream !== null && stopped[upstream] !== undefined) {
      delete stopped[upstream];
      moves.push({ from: upstream, to: role, kind: "handoff" });
    } else if (upstream !== null) {
      moves.push({ from: upstream, to: role, kind: "arrival" });
    }
  }

  return { moves, watch: { stopped } };
}

/** One plain sentence for a move, for the live region and for tests. */
export function moveSentence(move: WorkMove, label: (role: string) => string): string {
  const to = label(move.to);
  const from = label(move.from);
  return move.kind === "handoff"
    ? `${from} passed work to ${to}.`
    : `${to} picked up new work.`;
}
