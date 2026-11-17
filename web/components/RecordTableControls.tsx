import { Button } from "./Button";

/** Keep every live-run record table bounded to the same readable page size. */
export const RECORD_PAGE_SIZE = 5;

/**
 * Shared disclosure control for live-run record tables. The visible label is
 * deliberately only Open/Close; the accessible name identifies the table.
 */
export function RecordTableControls({
  label,
  summary,
  open,
  controls,
  onToggle,
}: {
  label: string;
  summary: string;
  open: boolean;
  controls: string;
  onToggle: () => void;
}) {
  return (
    <div className="record-table-controls" data-open={open}>
      <span className="micro">{summary}</span>
      <Button
        size="xs"
        variant="outline"
        aria-label={`${open ? "Close" : "Open"} ${label}`}
        aria-expanded={open}
        aria-controls={controls}
        onClick={onToggle}
      >
        {open ? "Close" : "Open"}
      </Button>
    </div>
  );
}

/** The common previous/page/next footer used by every paginated record table. */
export function RecordTablePager({
  label,
  page,
  totalPages,
  previousLabel,
  nextLabel,
  onPage,
}: {
  label: string;
  page: number;
  totalPages: number;
  previousLabel: string;
  nextLabel: string;
  onPage: (page: number) => void;
}) {
  if (totalPages <= 1) return null;

  return (
    <nav className="record-table-pager" aria-label={`${label} pages`}>
      <Button
        size="xs"
        variant="outline"
        disabled={page === 1}
        onClick={() => onPage(Math.max(1, page - 1))}
      >
        {previousLabel}
      </Button>
      <span className="micro" aria-live="polite">
        Page {page} of {totalPages}
      </span>
      <Button
        size="xs"
        variant="outline"
        disabled={page === totalPages}
        onClick={() => onPage(Math.min(totalPages, page + 1))}
      >
        {nextLabel}
      </Button>
    </nav>
  );
}
