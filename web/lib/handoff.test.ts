import { describe, expect, it } from "vitest";
import { detectWorkMoves, moveSentence } from "./handoff";
import { parseRunSnapshot } from "./parse";
import { snapshotFixture } from "./fixtures";
import type { AgentPanel } from "./types";

function panel(role: string, task: string | null): AgentPanel {
  const seed = parseRunSnapshot(snapshotFixture()).agents[1]!;
  return {
    ...seed,
    role,
    state: task === null ? "idle" : "thinking",
    current: task === null ? null : { ...seed.current!, id: task },
  };
}

const run = (tasks: Record<string, string | null>) =>
  Object.entries(tasks).map(([role, task]) => panel(role, task));

describe("observed worker starts", () => {
  it("does not infer a worker-to-worker transfer from coincident task changes", () => {
    expect(
      detectWorkMoves(
        run({ generation: "g1", reflection: null }),
        run({ generation: null, reflection: "r1" }),
      ),
    ).toEqual([{ from: "supervisor", to: "reflection", kind: "arrival" }]);
  });

  it("reports concurrent starts independently, including new roles", () => {
    const after = run({
      reflection: "r1",
      evolution: "e1",
      custom_worker: "c1",
    });
    expect(detectWorkMoves(run({}), after).map((move) => move.to)).toEqual([
      "reflection",
      "evolution",
      "custom_worker",
    ]);
  });

  it("does not animate a role continuing with its own next task", () => {
    expect(
      detectWorkMoves(run({ ranking: "k1" }), run({ ranking: "k2" })),
    ).toEqual([]);
  });

  it("reports nothing for unchanged snapshots, finishes or state-only changes", () => {
    const panels = run({ generation: "g1" });
    expect(detectWorkMoves(panels, panels)).toEqual([]);
    expect(detectWorkMoves(panels, run({ generation: null }))).toEqual([]);
    expect(
      detectWorkMoves(run({ generation: null }), [
        { ...panel("generation", null), state: "thinking" },
      ]),
    ).toEqual([]);
  });

  it("does not treat the supervisor as a worker", () => {
    expect(detectWorkMoves(run({}), run({ supervisor: "s1" }))).toEqual([]);
  });

  it("announces only the observed start, not a supposed transfer", () => {
    expect(
      moveSentence(
        { from: "supervisor", to: "reflection", kind: "arrival" },
        (role) => (role === "reflection" ? "Reflection" : "Supervisor"),
      ),
    ).toBe("Reflection picked up new work.");
  });
});
