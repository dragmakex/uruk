/**
 * Where each candidate sits now against where it sat at the previous
 * snapshot.
 *
 * Movement is read from the API's own ordering (best rating first, unrated
 * last, ties by id), so a row only ever claims to have moved because the
 * service put it somewhere else — never because the page guessed at a
 * trend. A candidate that was not on the previous board is `new`, which is
 * a different statement from `+2` and is shown differently.
 */

export type MovementKind = "up" | "down" | "same" | "new";

export interface RankMove {
  kind: MovementKind;
  /** Places gained since the previous snapshot; 0 for `same` and `new`. */
  gained: number;
  /** Places held at the previous snapshot, 0-based; null when new. */
  previous: number | null;
}

/** Positions from the order a list was last seen in. */
export function orderOf(items: ReadonlyArray<{ id: string }>): string[] {
  return items.map((item) => item.id);
}

/**
 * Compare the previous order with the current one.
 *
 * `gained` is positive when a candidate climbed. With `previous` order
 * [a, b, c] and current [b, a, c], `a` gained -1 and `b` gained 1.
 */
export function rankMovement(
  previous: readonly string[],
  current: ReadonlyArray<{ id: string }>,
): Map<string, RankMove> {
  const held = new Map(previous.map((id, index) => [id, index]));
  const moves = new Map<string, RankMove>();

  current.forEach((item, index) => {
    const was = held.get(item.id);
    if (was === undefined) {
      moves.set(item.id, { kind: "new", gained: 0, previous: null });
      return;
    }
    const gained = was - index;
    moves.set(item.id, {
      kind: gained > 0 ? "up" : gained < 0 ? "down" : "same",
      gained,
      previous: was,
    });
  });

  return moves;
}

export interface MoveGlyph {
  kind: MovementKind;
  /** The visible mark, e.g. `▲2`. */
  mark: string;
  /** The same fact in words, for the screen-reader-only companion. */
  label: string;
}

/** The arrow and the number for a move, or null when nothing happened. */
export function moveGlyph(move: RankMove | undefined): MoveGlyph | null {
  if (move === undefined) return null;
  const places = Math.abs(move.gained);
  switch (move.kind) {
    case "new":
      return { kind: "new", mark: "new", label: "not on the board at the last update" };
    case "up":
      return {
        kind: "up",
        mark: `▲${places}`,
        label: `up ${places} place${places === 1 ? "" : "s"} since the last update`,
      };
    case "down":
      return {
        kind: "down",
        mark: `▼${places}`,
        label: `down ${places} place${places === 1 ? "" : "s"} since the last update`,
      };
    default:
      return null;
  }
}
