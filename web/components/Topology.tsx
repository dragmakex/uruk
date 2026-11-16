"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { AgentPanel, Stats } from "@/lib/types";
import { ROLE_ORDER, WIRES } from "@/lib/roles";
import {
  detectWorkMoves,
  emptyWatch,
  type HandoffWatch,
  type WorkMove,
  type WorkMoveKind,
} from "@/lib/handoff";
import { AgentCard } from "./AgentCard";

interface Wire {
  key: string;
  points: string;
  /** The same elbow as an SVG path, for motion along the wire. */
  path: string;
  carrying: boolean;
}

/** A work move in flight along one wire. Retired when its dot lands. */
interface Pulse extends WorkMove {
  id: number;
}

/** How long a dot takes to travel its wire. Matches the CSS duration. */
const PULSE_MS = 900;

/**
 * The connected agent topology: Supervisor in the left column, the worker
 * grid on the right, with an orthogonal SVG wire layer measured from the
 * real card positions. Wires are decorative (aria-hidden) and hidden below
 * 1024px, where each card shows its textual `feeds` context instead. The
 * DOM stays one semantic list of agent sections at every breakpoint.
 */
export function Topology({
  runId,
  agents,
  stats,
  onMove,
}: {
  runId: string;
  agents: AgentPanel[];
  stats: Stats;
  /** Called once per observed work move, for the page's live region. */
  onMove?: (move: WorkMove) => void;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [wires, setWires] = useState<Wire[]>([]);
  const [pulses, setPulses] = useState<Pulse[]>([]);

  // The previous snapshot and the releases seen since it. Held in refs so
  // that observing a move never triggers the render it would compare to.
  const previous = useRef<readonly AgentPanel[] | null>(null);
  const watch = useRef<HandoffWatch>(emptyWatch());
  const pulseId = useRef(0);
  const landings = useRef<ReturnType<typeof setTimeout>[]>([]);

  useEffect(() => {
    const before = previous.current;
    previous.current = agents;
    // The first snapshot is a starting position, not a move.
    if (before === null) return;

    const seen = detectWorkMoves(before, agents, watch.current, Date.now());
    watch.current = seen.watch;
    if (seen.moves.length === 0) return;

    const born = seen.moves.map((move) => ({ ...move, id: (pulseId.current += 1) }));
    setPulses((held) => [...held, ...born]);
    for (const move of seen.moves) onMove?.(move);

    const timers = born.map((pulse) =>
      setTimeout(() => {
        setPulses((held) => held.filter((p) => p.id !== pulse.id));
      }, PULSE_MS),
    );
    landings.current.push(...timers);
  }, [agents, onMove]);

  useEffect(() => {
    const timers = landings;
    return () => {
      for (const timer of timers.current) clearTimeout(timer);
      timers.current = [];
    };
  }, []);

  const byRole = new Map(agents.map((a) => [a.role, a]));
  const supervisor = byRole.get("supervisor");
  const workers: AgentPanel[] = [];
  for (const role of ROLE_ORDER) {
    if (role === "supervisor") continue;
    const panel = byRole.get(role);
    if (panel) workers.push(panel);
  }
  // Roles outside the canonical order (future plans) still render.
  for (const panel of agents) {
    if (panel.role !== "supervisor" && !workers.includes(panel)) {
      workers.push(panel);
    }
  }

  const thinking = new Set(
    agents.filter((a) => a.state === "thinking").map((a) => a.role),
  );

  const measure = useCallback(() => {
    const container = containerRef.current;
    if (!container) return;
    const origin = container.getBoundingClientRect();
    if (origin.width === 0) return;

    const boxes = new Map<string, DOMRect>();
    container.querySelectorAll<HTMLElement>("[data-role]").forEach((el) => {
      const role = el.dataset.role;
      if (role) boxes.set(role, el.getBoundingClientRect());
    });

    const next: Wire[] = [];
    for (const [from, to] of WIRES) {
      const a = boxes.get(from);
      const b = boxes.get(to);
      if (!a || !b) continue;

      const points: string[] = [];
      if (Math.abs(a.left - b.left) < 8) {
        // Vertically stacked cards: a straight vertical drop.
        const x = a.left - origin.left + a.width / 2;
        points.push(`${x},${a.bottom - origin.top}`, `${x},${b.top - origin.top}`);
      } else {
        // Side by side: out of the source's right edge, elbow at the
        // midpoint, into the target's left edge.
        const x1 = a.right - origin.left;
        const y1 = a.top - origin.top + a.height / 2;
        const x2 = b.left - origin.left;
        const y2 = b.top - origin.top + b.height / 2;
        const mid = x1 + (x2 - x1) / 2;
        points.push(
          `${x1},${y1}`,
          `${mid},${y1}`,
          `${mid},${y2}`,
          `${x2},${y2}`,
        );
      }
      next.push({
        key: `${from}-${to}`,
        points: points.join(" "),
        path: `M${points.join(" L")}`,
        carrying: thinking.has(to),
      });
    }
    setWires(next);
    // The wire set depends only on geometry and which roles are thinking.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents]);

  useEffect(() => {
    measure();
    const container = containerRef.current;
    if (!container || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => measure());
    observer.observe(container);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [measure]);

  return (
    <div className="topology" ref={containerRef}>
      <svg className="wires" aria-hidden="true">
        {wires.map((wire) => (
          <polyline
            key={wire.key}
            points={wire.points}
            className={wire.carrying ? "is-carrying" : undefined}
          />
        ))}
        {pulses.map((pulse) => {
          const wire = wires.find((w) => w.key === `${pulse.from}-${pulse.to}`);
          if (!wire) return null;
          return (
            <circle key={pulse.id} className="handoff-pulse" r={2.5}>
              <animateMotion dur={`${PULSE_MS}ms`} path={wire.path} fill="freeze" />
            </circle>
          );
        })}
      </svg>
      {supervisor ? <AgentCard runId={runId} panel={supervisor} stats={stats} /> : <div />}
      <div className="workers">
        {workers.map((panel) => (
          <AgentCard key={panel.role} runId={runId} panel={panel} stats={stats} />
        ))}
      </div>
    </div>
  );
}
