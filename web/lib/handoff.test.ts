import { describe, expect, it } from "vitest";
import {
  detectWorkMoves,
  emptyWatch,
  HANDOFF_WINDOW_MS,
  moveSentence,
  type HandoffWatch,
} from "./handoff";
import type { AgentPanel, AgentState } from "./types";

const counts = {
  pending: 0,
  blocked: 0,
  running: 0,
  completed: 0,
  failed: 0,
  cancelled: 0,
  uncertain: 0,
};

/** A panel whose only interesting fact is which task it is running. */
function panel(role: string, taskId: string | null, state?: AgentState): AgentPanel {
  return {
    role,
    state: state ?? (taskId === null ? "idle" : "thinking"),
    counts,
    current:
      taskId === null
        ? null
        : {
            id: taskId,
            reasoning_preview: null,
            reasoning_streaming: false,
            strategy: "draft",
            state: "running",
            attempts: 1,
            started_at: "2026-10-04T14:00:00Z",
            finished_at: null,
            error: null,
          },
    last_finished: null,
    assignments: null,
  };
}

const run = (patches: Record<string, string | null>): AgentPanel[] =>
  Object.entries(patches).map(([role, task]) => panel(role, task));

const T0 = 1_700_000_000_000;

/** Feed a sequence of snapshots through the watcher, as the stream would. */
function timeline(
  snapshots: Array<{ at: number; panels: AgentPanel[] }>,
): ReturnType<typeof detectWorkMoves>["moves"][] {
  let watch: HandoffWatch = emptyWatch();
  const seen: ReturnType<typeof detectWorkMoves>["moves"][] = [];
  for (let index = 1; index < snapshots.length; index += 1) {
    const step = detectWorkMoves(
      snapshots[index - 1]!.panels,
      snapshots[index]!.panels,
      watch,
      snapshots[index]!.at,
    );
    watch = step.watch;
    seen.push(step.moves);
  }
  return seen;
}

describe("detectWorkMoves", () => {
  it("sees a handoff when the upstream let go as the downstream picked up", () => {
    const step = detectWorkMoves(
      run({ generation: "task_g1" }),
      run({ generation: null, reflection: "task_r1" }),
      emptyWatch(),
      T0,
    );
    expect(step.moves).toEqual([{ from: "generation", to: "reflection", kind: "handoff" }]);
  });

  it("still sees it when the release and the arrival fall in different snapshots", () => {
    const steps = timeline([
      { at: T0, panels: run({ generation: "task_g1" }) },
      // The task finished; nothing has started yet.
      { at: T0 + 2_000, panels: run({ generation: null }) },
      { at: T0 + 5_000, panels: run({ reflection: "task_r1" }) },
    ]);
    expect(steps[0]).toEqual([]);
    expect(steps[1]).toEqual([
      { from: "generation", to: "reflection", kind: "handoff" },
    ]);
  });

  it("forgets a release older than the window", () => {
    const steps = timeline([
      { at: T0, panels: run({ generation: "task_g1" }) },
      { at: T0 + 1_000, panels: run({ generation: null }) },
      {
        at: T0 + 1_000 + HANDOFF_WINDOW_MS + 1,
        panels: run({ reflection: "task_r1" }),
      },
    ]);
    expect(steps[1]).toEqual([
      { from: "generation", to: "reflection", kind: "arrival" },
    ]);
  });

  it("draws no wire when a panel swaps in its own next task", () => {
    const step = detectWorkMoves(
      run({ ranking: "task_k1" }),
      run({ ranking: "task_k9" }),
      emptyWatch(),
      T0,
    );
    // ranking finished its own task and started another: it carried on
    // rather than receiving work, so no wire is carrying anything new.
    expect(step.moves).toEqual([]);
  });

  it("calls work with no upstream release an arrival, named by the pipeline", () => {
    const step = detectWorkMoves(
      run({ generation: null }),
      run({ generation: "task_g4" }),
      emptyWatch(),
      T0,
    );
    expect(step.moves).toEqual([{ from: "supervisor", to: "generation", kind: "arrival" }]);
  });

  it("spends a release on the move it explains, not on the next one", () => {
    const steps = timeline([
      { at: T0, panels: run({ generation: "task_g1" }) },
      { at: T0 + 1_000, panels: run({ generation: null, reflection: "task_r1" }) },
      { at: T0 + 2_000, panels: run({ generation: null, reflection: null, proximity: "task_p1" }) },
    ]);
    expect(steps[0]).toEqual([
      { from: "generation", to: "reflection", kind: "handoff" },
    ]);
    expect(steps[1]).toEqual([
      { from: "supervisor", to: "proximity", kind: "arrival" },
    ]);
  });

  it("reports nothing when no panel's task changed", () => {
    const panels = run({ generation: "task_g1", reflection: "task_r1" });
    expect(detectWorkMoves(panels, panels, emptyWatch(), T0).moves).toEqual([]);
  });

  it("treats a role's first task as an arrival, not a handoff", () => {
    const step = detectWorkMoves(
      run({ generation: "task_g1" }),
      run({ generation: "task_g1", meta_review: "task_m1" }),
      emptyWatch(),
      T0,
    );
    expect(step.moves).toEqual([
      { from: "ranking", to: "meta_review", kind: "arrival" },
    ]);
  });

  it("leaves a self-swap out without spending an unrelated release", () => {
    const step = detectWorkMoves(
      run({ generation: "task_g1", reflection: "task_r1" }),
      run({ generation: null, reflection: "task_r2" }),
      emptyWatch(),
      T0,
    );
    expect(step.moves.filter((move) => move.to === "reflection")).toEqual([]);
  });
});

describe("moveSentence", () => {
  const label = (role: string) => role.charAt(0).toUpperCase() + role.slice(1);

  it("says both ends when both ends were seen", () => {
    expect(
      moveSentence({ from: "generation", to: "reflection", kind: "handoff" }, label),
    ).toBe("Generation passed work to Reflection.");
  });

  it("claims only the arrival when the departure was not seen", () => {
    expect(
      moveSentence({ from: "reflection", to: "ranking", kind: "arrival" }, label),
    ).toBe("Ranking picked up new work.");
  });
});
