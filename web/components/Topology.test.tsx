import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentPanel, Stats } from "@/lib/types";
import type { WorkMove } from "@/lib/handoff";
import { Topology } from "./Topology";

const stats: Stats = {
  items: 3,
  reviews: 2,
  matches: 0,
  decisions: 0,
  clusters: 0,
  sources: 1,
  works: 0,
  searches: 0,
  workers_busy: 1,
  workers_total: 4,
  leader: null,
};

const counts = {
  pending: 0,
  blocked: 0,
  running: 0,
  completed: 0,
  failed: 0,
  cancelled: 0,
  uncertain: 0,
};

function panel(role: string, taskId: string | null): AgentPanel {
  return {
    role,
    state: taskId === null ? "idle" : "thinking",
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

const agents = (tasks: Record<string, string | null>): AgentPanel[] =>
  ["supervisor", "generation", "reflection", "ranking", "proximity", "evolution", "meta_review"].map(
    (role) => panel(role, role === "supervisor" ? null : tasks[role] ?? null),
  );

/**
 * jsdom gives every element a zero box, which would make the wire layer
 * measure nothing. Lay the cards out on a grid so the geometry is real.
 */
function layout(): void {
  let row = 0;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      const role = this.dataset.role;
      if (!role) {
        // The container itself: origin and a width the wires can be drawn in.
        return box(0, 0, 900, 700);
      }
      if (role === "supervisor") return box(0, 0, 240, 120);
      row += 1;
      const column = row % 2 === 1 ? 0 : 1;
      const line = Math.floor((row - 1) / 2);
      return box(400 + column * 260, line * 160, 240, 120);
    },
  );
}

function box(left: number, top: number, width: number, height: number): DOMRect {
  return {
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

beforeEach(() => {
  layout();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("Topology", () => {
  it("draws a wire for every pipeline edge it can measure", () => {
    const { container } = render(
      <Topology runId="run_0412aa" agents={agents({ generation: "task_g1" })} stats={stats} />,
    );
    expect(container.querySelectorAll("svg.wires polyline")).toHaveLength(7);
  });

  it("dashes the wires into the roles that are thinking", () => {
    const { container } = render(
      <Topology runId="run_0412aa" agents={agents({ generation: "task_g1" })} stats={stats} />,
    );
    const carrying = Array.from(container.querySelectorAll("svg.wires polyline")).filter((el) =>
      el.classList.contains("is-carrying"),
    );
    // generation is busy, so its inbound wire carries.
    expect(carrying).toHaveLength(1);
  });

  it("sends a dot along the wire when work moves to another role", () => {
    const moves: WorkMove[] = [];
    const { container, rerender } = render(
      <Topology
        runId="run_0412aa"
        agents={agents({ generation: "task_g1" })}
        stats={stats}
        onMove={(move) => moves.push(move)}
      />,
    );
    expect(container.querySelector(".handoff-pulse")).toBeNull();

    rerender(
      <Topology
        runId="run_0412aa"
        agents={agents({ reflection: "task_r1" })}
        stats={stats}
        onMove={(move) => moves.push(move)}
      />,
    );

    const dot = container.querySelector("circle.handoff-pulse");
    expect(dot).not.toBeNull();
    expect(dot?.querySelector("animateMotion")?.getAttribute("path")).toMatch(/^M/);
    expect(moves).toEqual([{ from: "generation", to: "reflection", kind: "handoff" }]);
  });

  it("draws nothing on the first snapshot, which is a position, not a move", () => {
    const moves: WorkMove[] = [];
    const { container } = render(
      <Topology
        runId="run_0412aa"
        agents={agents({ generation: "task_g1", reflection: "task_r1" })}
        stats={stats}
        onMove={(move) => moves.push(move)}
      />,
    );
    expect(container.querySelector(".handoff-pulse")).toBeNull();
    expect(moves).toEqual([]);
  });

  it("retires the dot once it has landed", () => {
    vi.useFakeTimers();
    const { container, rerender } = render(
      <Topology runId="run_0412aa" agents={agents({ generation: "task_g1" })} stats={stats} />,
    );
    rerender(
      <Topology runId="run_0412aa" agents={agents({ reflection: "task_r1" })} stats={stats} />,
    );
    expect(container.querySelectorAll(".handoff-pulse")).toHaveLength(1);

    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(container.querySelector(".handoff-pulse")).toBeNull();
  });

  it("leaves the cards as one readable list of roles", () => {
    render(<Topology runId="run_0412aa" agents={agents({})} stats={stats} />);
    for (const role of ["Supervisor", "Generation", "Reflection", "Ranking"]) {
      expect(screen.getByText(role)).toBeDefined();
    }
  });
});
