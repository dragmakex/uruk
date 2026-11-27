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
  [
    "supervisor",
    "generation",
    "reflection",
    "ranking",
    "proximity",
    "evolution",
    "meta_review",
  ].map((role) =>
    panel(role, role === "supervisor" ? null : (tasks[role] ?? null)),
  );

/**
 * jsdom gives every element a zero box, which would make the wire layer
 * measure nothing. Lay the cards out on a grid so the geometry is real.
 */
function layout(): void {
  const workerRoles = [
    "generation",
    "reflection",
    "ranking",
    "proximity",
    "evolution",
    "meta_review",
  ];
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      const role = this.dataset.role;
      if (!role) return box(0, 0, 900, 700);
      if (role === "supervisor") return box(24, 0, 520, 140);
      const index = workerRoles.indexOf(role);
      return box(
        24 + (index % 3) * 280,
        200 + Math.floor(index / 3) * 180,
        260,
        140,
      );
    },
  );
}

function box(
  left: number,
  top: number,
  width: number,
  height: number,
): DOMRect {
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
  it("draws only supervisor allocation connections to the measured workers", () => {
    const { container } = render(
      <Topology
        runId="run_0412aa"
        agents={agents({ generation: "task_g1" })}
        stats={stats}
      />,
    );
    const wires = container.querySelectorAll("svg.wires polyline");
    expect(wires).toHaveLength(6);
    for (const wire of wires)
      expect(wire).toHaveAttribute("data-from", "supervisor");
  });

  it("dashes the wires into the roles that are thinking", () => {
    const { container } = render(
      <Topology
        runId="run_0412aa"
        agents={agents({ generation: "task_g1" })}
        stats={stats}
      />,
    );
    const carrying = Array.from(
      container.querySelectorAll("svg.wires polyline"),
    ).filter((el) => el.classList.contains("is-carrying"));
    // generation is busy, so its inbound wire carries.
    expect(carrying).toHaveLength(1);
  });

  it("marks an observed worker start without claiming a worker-to-worker handoff", () => {
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
    expect(dot?.querySelector("animateMotion")?.getAttribute("path")).toMatch(
      /^M/,
    );
    expect(moves).toEqual([
      { from: "supervisor", to: "reflection", kind: "arrival" },
    ]);
  });

  it("starts each dynamically inserted pulse once on the existing SVG clock", () => {
    const begin = vi.fn();
    const previous = Object.getOwnPropertyDescriptor(
      SVGElement.prototype,
      "beginElement",
    );
    Object.defineProperty(SVGElement.prototype, "beginElement", {
      configurable: true,
      value: begin,
    });
    try {
      const { container, rerender } = render(
        <Topology
          runId="run_0412aa"
          agents={agents({ generation: "task_g1" })}
          stats={stats}
        />,
      );
      rerender(
        <Topology
          runId="run_0412aa"
          agents={agents({ reflection: "task_r1" })}
          stats={stats}
        />,
      );
      expect(container.querySelector("animateMotion")).toHaveAttribute(
        "begin",
        "indefinite",
      );
      expect(begin).toHaveBeenCalledTimes(1);
      rerender(
        <Topology
          runId="run_0412aa"
          agents={agents({ reflection: "task_r1" })}
          stats={stats}
        />,
      );
      expect(begin).toHaveBeenCalledTimes(1);
    } finally {
      if (previous)
        Object.defineProperty(SVGElement.prototype, "beginElement", previous);
      else Reflect.deleteProperty(SVGElement.prototype, "beginElement");
    }
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
      <Topology
        runId="run_0412aa"
        agents={agents({ generation: "task_g1" })}
        stats={stats}
      />,
    );
    rerender(
      <Topology
        runId="run_0412aa"
        agents={agents({ reflection: "task_r1" })}
        stats={stats}
      />,
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
