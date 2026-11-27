"use client";

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { AgentPanel, Stats } from "@/lib/types";
import { ROLE_ORDER } from "@/lib/roles";
import { detectWorkMoves, type WorkMove } from "@/lib/handoff";
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

/** Both the SMIL duration and the dot's lifetime. */
const PULSE_MS = 900;

function startPulse(motion: SVGElement | null) {
  // Inserted SMIL nodes otherwise start at the parent SVG's time zero.
  if (
    motion &&
    "beginElement" in motion &&
    typeof motion.beginElement === "function"
  ) {
    motion.beginElement();
  }
}

/**
 * The supervisor sits above an asynchronous worker pool. Measured fan-out
 * connections indicate supervision, not a required worker-to-worker order.
 * Decorative wires disappear on phones; the semantic grouping remains.
 */
export function Topology({
  runId,
  agents,
  stats,
  onMove,
  supervisorDetails,
}: {
  runId: string;
  agents: AgentPanel[];
  stats: Stats;
  /** Called once per observed work move, for the page's live region. */
  onMove?: (move: WorkMove) => void;
  supervisorDetails?: ReactNode;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [wires, setWires] = useState<Wire[]>([]);
  const [pulses, setPulses] = useState<Pulse[]>([]);

  // The first snapshot is a position, not an observed task start.
  const previous = useRef<readonly AgentPanel[] | null>(null);
  const pulseId = useRef(0);
  const landings = useRef<ReturnType<typeof setTimeout>[]>([]);

  useEffect(() => {
    const before = previous.current;
    previous.current = agents;
    // The first snapshot is a starting position, not a move.
    if (before === null) return;

    const seen = detectWorkMoves(before, agents);
    if (seen.length === 0) return;

    const born = seen.map((move) => ({ ...move, id: (pulseId.current += 1) }));
    setPulses((held) => [...held, ...born]);
    for (const move of seen) onMove?.(move);

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
    const a = boxes.get("supervisor");
    for (const [role, b] of boxes) {
      if (role === "supervisor") continue;
      if (!a || !b) continue;

      // A left-hand bus reaches every row through its gutter, never
      // crossing another worker card to imply a serial handoff.
      const sourceX = a.left - origin.left + a.width / 2;
      const sourceY = a.bottom - origin.top;
      const targetX = b.left - origin.left + b.width / 2;
      const targetY = b.top - origin.top;
      const points = [
        `${sourceX},${sourceY}`,
        `${sourceX},${sourceY + 12}`,
        `4,${sourceY + 12}`,
        `4,${targetY - 16}`,
        `${targetX},${targetY - 16}`,
        `${targetX},${targetY}`,
      ];
      next.push({
        key: `supervisor-${role}`,
        points: points.join(" "),
        path: `M${points.join(" L")}`,
        carrying: thinking.has(role),
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
            data-from="supervisor"
            data-to={wire.key.slice("supervisor-".length)}
            points={wire.points}
            className={wire.carrying ? "is-carrying" : undefined}
          />
        ))}
        {pulses.map((pulse) => {
          const wire = wires.find((w) => w.key === `${pulse.from}-${pulse.to}`);
          if (!wire) return null;
          return (
            <circle key={pulse.id} className="handoff-pulse" r={2.5}>
              <animateMotion
                ref={startPulse}
                begin="indefinite"
                dur={`${PULSE_MS}ms`}
                path={wire.path}
                fill="freeze"
              />
            </circle>
          );
        })}
      </svg>
      <div className="topology-supervisor">
        {supervisor && (
          <AgentCard runId={runId} panel={supervisor} stats={stats} />
        )}
        {supervisorDetails}
      </div>
      <section className="worker-pool" aria-label="Async worker pool">
        <p className="pool-label micro">
          Specialized workers · shared context, independent tasks
        </p>
        <div className="workers">
          {workers.map((panel) => (
            <AgentCard
              key={panel.role}
              runId={runId}
              panel={panel}
              stats={stats}
            />
          ))}
        </div>
        {workers.length === 0 && <p>No workers selected for this plan.</p>}
      </section>
    </div>
  );
}
