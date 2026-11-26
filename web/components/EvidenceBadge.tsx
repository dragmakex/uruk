"use client";

import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent } from "react";
import { createPortal } from "react-dom";
import type { ItemCard } from "@/lib/types";
import "./evidence-badge.css";
import { Button } from "./Button";

/** A recorded assessment with its exact basis; ratings never choose the tier. */
export function EvidenceBadge({ item }: { item: ItemCard }) {
  const id = useId();
  const button = useRef<HTMLButtonElement>(null);
  const tooltip = useRef<HTMLDivElement>(null);
  const delay = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [position, setPosition] = useState<{ left: number; top: number; maxHeight: number } | null>(null);
  const open = position !== null;
  const basis = item.assessment_basis;

  function show() {
    if (delay.current) clearTimeout(delay.current);
    const rect = button.current!.getBoundingClientRect();
    const below = window.innerHeight - rect.bottom;
    setPosition({
      left: Math.max(12, Math.min(rect.left, window.innerWidth - Math.min(340, window.innerWidth - 24) - 12)),
      top: below >= 240 ? rect.bottom + 4 : Math.max(12, rect.top - 244),
      maxHeight: below >= 240 ? Math.min(340, below - 16) : Math.min(240, Math.max(80, rect.top - 24)),
    });
  }
  function leave() {
    delay.current = setTimeout(() => setPosition(null), 150);
  }
  useEffect(() => () => { if (delay.current) clearTimeout(delay.current); }, []);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: KeyboardEvent) => { if (event.key === "Escape") setPosition(null); };
    const outside = (event: PointerEvent) => {
      if (!button.current?.contains(event.target as Node) && !tooltip.current?.contains(event.target as Node)) setPosition(null);
    };
    const resize = () => setPosition(null);
    document.addEventListener("keydown", dismiss);
    document.addEventListener("pointerdown", outside);
    window.addEventListener("resize", resize);
    return () => {
      document.removeEventListener("keydown", dismiss);
      document.removeEventListener("pointerdown", outside);
      window.removeEventListener("resize", resize);
    };
  }, [open]);

  return (
    <>
      <Button ref={button} variant="outline" size="xs" className="evidence-badge" data-tier={item.assessment}
        aria-label={`Evidence for ${item.title}: ${item.assessment}`}
        aria-describedby={open ? id : undefined}
        onMouseEnter={show} onMouseLeave={leave} onFocus={show}
        onBlur={() => setPosition(null)}
        onKeyDown={(event: ReactKeyboardEvent<HTMLButtonElement>) => { if (event.key === "Escape") setPosition(null); }}
        onClick={(event: MouseEvent<HTMLButtonElement>) => { event.preventDefault(); event.stopPropagation(); show(); }}>
        {item.assessment}{item.assessment === "supported" ? " / evidence-backed" : ""}
      </Button>
      {open && createPortal(
        <div ref={tooltip} id={id} role="tooltip" className="evidence-tooltip" style={position}
          onMouseEnter={() => { if (delay.current) clearTimeout(delay.current); }} onMouseLeave={leave}>
          <strong>{item.assessment}</strong>
          <p>{basis?.rationale ?? (item.assessment === "untested"
            ? "No validation has been recorded."
            : "No assessment basis is recorded. This is not proof of the claim.")}</p>
          {basis && <>
            <p className="micro">{basis.review_id ? `Review ${basis.review_id}` : basis.decision_id ? `Decision ${basis.decision_id}` : "No review or decision recorded"}</p>
            {basis.evidence.length === 0 ? <p>No evidence cited in this assessment. Agent opinion cannot establish support.</p> : (
              <ul>{basis.evidence.map((e, i) => <li key={`${e.id}-${i}`}>
                <strong>{e.label || e.id}</strong> <span className="micro">({e.id})</span>
                <p>{e.kind.replaceAll("_", " ")}: {e.relation} “{e.claim}”</p>
                {e.limits && <p>Limits: {e.limits}</p>}
              </li>)}</ul>
            )}
          </>}
        </div>, document.body,
      )}
    </>
  );
}
