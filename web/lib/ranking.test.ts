import { describe, expect, it } from "vitest";
import { moveGlyph, orderOf, rankMovement } from "./ranking";

const items = (...ids: string[]) => ids.map((id) => ({ id }));

describe("rankMovement", () => {
  it("counts the places a candidate climbed", () => {
    const moves = rankMovement(["a", "b", "c"], items("b", "a", "c"));
    expect(moves.get("b")).toEqual({ kind: "up", gained: 1, previous: 1 });
    expect(moves.get("a")).toEqual({ kind: "down", gained: -1, previous: 0 });
  });

  it("counts a long climb, not just adjacency", () => {
    const moves = rankMovement(["a", "b", "c", "d"], items("d", "a", "b", "c"));
    expect(moves.get("d")).toEqual({ kind: "up", gained: 3, previous: 3 });
    expect(moves.get("a")).toEqual({ kind: "down", gained: -1, previous: 0 });
  });

  it("says nothing moved when the order is unchanged", () => {
    const moves = rankMovement(["a", "b"], items("a", "b"));
    expect(moves.get("a")).toEqual({ kind: "same", gained: 0, previous: 0 });
    expect(moves.get("b")).toEqual({ kind: "same", gained: 0, previous: 1 });
  });

  it("marks a candidate that was not on the previous board as new", () => {
    const moves = rankMovement(["a", "b"], items("a", "c", "b"));
    expect(moves.get("c")).toEqual({ kind: "new", gained: 0, previous: null });
    // b was pushed down by a newcomer, which is a real move worth showing.
    expect(moves.get("b")).toEqual({ kind: "down", gained: -1, previous: 1 });
  });

  it("handles a candidate leaving the board", () => {
    const moves = rankMovement(["a", "b", "c"], items("a", "c"));
    expect([...moves.keys()].sort()).toEqual(["a", "c"]);
    expect(moves.get("c")).toEqual({ kind: "up", gained: 1, previous: 2 });
  });

  it("reads an empty previous board as everything being new", () => {
    const moves = rankMovement([], items("a"));
    expect(moves.get("a")?.kind).toBe("new");
  });
});

describe("moveGlyph", () => {
  it("writes the arrow and the places for each direction", () => {
    expect(moveGlyph({ kind: "up", gained: 2, previous: 3 })).toEqual({
      kind: "up",
      mark: "▲2",
      label: "up 2 places since the last update",
    });
    expect(moveGlyph({ kind: "down", gained: -1, previous: 0 })).toEqual({
      kind: "down",
      mark: "▼1",
      label: "down 1 place since the last update",
    });
  });

  it("uses the singular for a single place", () => {
    expect(moveGlyph({ kind: "up", gained: 1, previous: 2 })?.label).toBe(
      "up 1 place since the last update",
    );
  });

  it("shows nothing for a row that held its place", () => {
    expect(moveGlyph({ kind: "same", gained: 0, previous: 1 })).toBeNull();
    expect(moveGlyph(undefined)).toBeNull();
  });

  it("labels a newcomer as a statement about the board, not a trend", () => {
    expect(moveGlyph({ kind: "new", gained: 0, previous: null })).toEqual({
      kind: "new",
      mark: "new",
      label: "not on the board at the last update",
    });
  });
});

describe("orderOf", () => {
  it("keeps the order the service sent", () => {
    expect(orderOf(items("c", "a", "b"))).toEqual(["c", "a", "b"]);
  });
});
