import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { CandidateHistory } from "./CandidateHistory";
import type { ItemCard } from "@/lib/types";
import { formatElo } from "@/lib/format";
import { moveGlyph, orderOf, rankMovement, type RankMove } from "@/lib/ranking";
import {
  RECORD_PAGE_SIZE,
  RecordTableControls,
  RecordTablePager,
} from "./RecordTableControls";

/** How long a row takes to slide to its new place. */
const SLIDE_MS = 420;

function reducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/**
 * Ranking cards: candidates best-first as the API orders them, showing the
 * recorded rating, match count, assessment, and review facts. Unrated
 * candidates say so instead of showing an invented number.
 *
 * When the service reorders the board, each row that changed place slides
 * from where it was and carries the number of places it gained or lost
 * since the previous snapshot. The count stays until the next reorder, so
 * a reader arriving late still sees which way the last result pushed it.
 *
 * Each row is a disclosure: opening it reads that candidate's own match
 * history, so the board answers "why is this one first?" in the record's
 * words rather than in a summary of them.
 */
export function RankingList({
  runId,
  items,
}: {
  runId: string;
  items: ItemCard[];
}) {
  const rows = useRef(new Map<string, HTMLElement>());
  const [open, setOpen] = useState(false);
  const [requestedPage, setRequestedPage] = useState(1);
  /** Candidates whose history has been asked for at least once. */
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  const [disclosureRevision, setDisclosureRevision] = useState(0);
  const disclosureChanged = useRef(false);
  const tops = useRef(new Map<string, number>());
  const previousOrder = useRef<string[] | null>(null);
  const [movement, setMovement] = useState<Map<string, RankMove>>(() => new Map());
  const order = orderOf(items);
  const placed = order.join("\u0000");
  const totalPages = Math.max(1, Math.ceil(items.length / RECORD_PAGE_SIZE));
  const page = Math.min(requestedPage, totalPages);
  const pageStart = (page - 1) * RECORD_PAGE_SIZE;
  const visibleItems = items.slice(pageStart, pageStart + RECORD_PAGE_SIZE);

  // Movement is recorded when the board is actually reordered, not on every
  // snapshot: the count then stays put until a later result changes it, so
  // someone arriving between matches still sees which way the last one
  // pushed each row.
  useEffect(() => {
    const before = previousOrder.current;
    previousOrder.current = order;
    if (before === null || before.join("\u0000") === placed) return;
    setMovement(rankMovement(before, items));
    // `order` is rebuilt each snapshot; `placed` is its stable identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [placed]);

  // FLIP: measure where the rows were, let React place them, then animate
  // each one back from its old offset. Cheap, and it uses the same layout
  // the browser already computed.
  useLayoutEffect(() => {
    const next = new Map<string, number>();
    const historyOpen = Array.from(rows.current.values()).some(
      (row) => row.closest("details")?.open,
    );
    const suppressMotion = disclosureChanged.current || historyOpen;
    for (const [id, el] of rows.current) {
      // Document coordinates stay stable while the reader scrolls. Viewport
      // coordinates made an SSE refresh look like a rank change after scroll.
      const top = el.getBoundingClientRect().top + window.scrollY;
      next.set(id, top);
      const was = tops.current.get(id);
      if (was === undefined || Math.abs(was - top) < 1) continue;
      // Expanding or collapsing a history changes the board's height. Do not
      // mix that disclosure reflow with rank motion.
      if (suppressMotion) continue;
      if (reducedMotion() || typeof el.animate !== "function") continue;
      el.animate(
        [{ transform: `translateY(${was - top}px)` }, { transform: "none" }],
        { duration: SLIDE_MS, easing: "cubic-bezier(.2,.7,.3,1)" },
      );
    }
    tops.current = next;
    disclosureChanged.current = false;
  }, [disclosureRevision, open, page, placed]);

  if (items.length === 0) {
    return (
      <p className="micro">
        No candidates yet. Items appear here once Generation drafts them.
      </p>
    );
  }

  return (
    <div className="record-table-shell">
      <RecordTableControls
        label="candidates"
        summary={`${items.length} ${items.length === 1 ? "candidate" : "candidates"} ranked`}
        open={open}
        controls="candidate-table"
        onToggle={() => setOpen((visible) => !visible)}
      />
      <div id="candidate-table" hidden={!open}>
        {open && (
          <>
            <ol id="candidate-list" className="candidate-rows record-table-body">
              {visibleItems.map((item, index) => {
                const move = moveGlyph(movement.get(item.id));
                return (
                  <li key={item.id} className="item-entry">
                    <details
                      className="row-history"
                      onToggle={(event) => {
                        const isOpen = event.currentTarget.open;
                        disclosureChanged.current = true;
                        setDisclosureRevision((revision) => revision + 1);
                        if (isOpen) {
                          setOpened((prev) =>
                            prev.has(item.id) ? prev : new Set(prev).add(item.id),
                          );
                        }
                      }}
                    >
                      <summary
                        className="item-row"
                        ref={(el) => {
                          if (el) rows.current.set(item.id, el);
                          else rows.current.delete(item.id);
                        }}
                      >
                        <div className="item-main">
                          <span className="item-title">
                            {pageStart + index + 1}. {item.title}
                          </span>
                          <p className="item-meta micro">
                            {item.kind}, {item.assessment}
                            {item.disposition !== "active" ? `, ${item.disposition}` : ""}
                            {item.review_count > 0
                              ? `, ${item.review_count} review${item.review_count === 1 ? "" : "s"}`
                              : ", unreviewed"}
                          </p>
                        </div>
                        <div className="item-rating">
                          {item.rating !== null ? (
                            <>
                              <span className="micro-ink">
                                elo {formatElo(item.rating)}, {item.matches_played}{" "}
                                {item.matches_played === 1 ? "match" : "matches"}
                              </span>
                              {item.stale_rating && (
                                <span className="micro stale-flag"> STALE</span>
                              )}
                            </>
                          ) : (
                            <span className="micro">unrated</span>
                          )}
                          {/* A newcomer is usually unrated, so the movement mark cannot
                              live inside the rated branch. */}
                          {move !== null && (
                            <span className={`rank-move is-${move.kind}`}>
                              <span aria-hidden="true">{move.mark}</span>
                              <span className="visually-hidden"> {move.label}</span>
                            </span>
                          )}
                        </div>
                      </summary>
                      {opened.has(item.id) && (
                        <CandidateHistory runId={runId} itemId={item.id} />
                      )}
                    </details>
                  </li>
                );
              })}
            </ol>
            <RecordTablePager
              label="Candidates"
              page={page}
              totalPages={totalPages}
              previousLabel="Higher-ranked"
              nextLabel="Lower-ranked"
              onPage={setRequestedPage}
            />
          </>
        )}
      </div>
    </div>
  );
}
