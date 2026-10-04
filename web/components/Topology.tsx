"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { AgentPanel, Stats } from "@/lib/types";
import { ROLE_ORDER, WIRES } from "@/lib/roles";
import { AgentCard } from "./AgentCard";

interface Wire {
  key: string;
  points: string;
  carrying: boolean;
}

/**
 * The connected agent topology: Supervisor in the left column, the worker
 * grid on the right, with an orthogonal SVG wire layer measured from the
 * real card positions. Wires are decorative (aria-hidden) and hidden below
 * 1024px, where each card shows its textual `feeds` context instead. The
 * DOM stays one semantic list of agent sections at every breakpoint.
 */
export function Topology({
  agents,
  stats,
}: {
  agents: AgentPanel[];
  stats: Stats;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [wires, setWires] = useState<Wire[]>([]);

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

      let points: string;
      if (Math.abs(a.left - b.left) < 8) {
        // Vertically stacked cards: a straight vertical drop.
        const x = a.left - origin.left + a.width / 2;
        points = `${x},${a.bottom - origin.top} ${x},${b.top - origin.top}`;
      } else {
        // Side by side: out of the source's right edge, elbow at the
        // midpoint, into the target's left edge.
        const x1 = a.right - origin.left;
        const y1 = a.top - origin.top + a.height / 2;
        const x2 = b.left - origin.left;
        const y2 = b.top - origin.top + b.height / 2;
        const mid = x1 + (x2 - x1) / 2;
        points = `${x1},${y1} ${mid},${y1} ${mid},${y2} ${x2},${y2}`;
      }
      next.push({
        key: `${from}-${to}`,
        points,
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
      </svg>
      {supervisor ? <AgentCard panel={supervisor} stats={stats} /> : <div />}
      <div className="workers">
        {workers.map((panel) => (
          <AgentCard key={panel.role} panel={panel} stats={stats} />
        ))}
      </div>
    </div>
  );
}
