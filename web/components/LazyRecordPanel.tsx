"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import type { PageInfo, PageRequest } from "@/lib/types";
import { Button } from "./Button";
import {
  RECORD_PAGE_SIZE,
  RecordTableControls,
  RecordTablePager,
} from "./RecordTableControls";

/**
 * A disclosure owns one bounded page, never a prefetched collection. Reads
 * happen only in Open, pager, Refresh and Retry handlers. SSE updates merely
 * mark a cached read stale; closing aborts work and retains the last good page.
 * The parent keys this component by run identity.
 */
export function LazyRecordPanel<T extends PageInfo>({
  label,
  pagerLabel,
  controls,
  summary,
  revision,
  fetchPage,
  children,
}: {
  label: string;
  pagerLabel: string;
  controls: string;
  summary: string;
  revision: string;
  fetchPage: (request: PageRequest, signal: AbortSignal) => Promise<T>;
  children: (data: T) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [read, setRead] = useState<{
    status: "idle" | "reading" | "ready" | "failed";
    data: T | null;
    revision: string | null;
  }>({ status: "idle", data: null, revision: null });
  const controller = useRef<AbortController | null>(null);
  const generation = useRef(0);
  const requested = useRef<PageRequest>({ offset: 0, limit: RECORD_PAGE_SIZE });

  useEffect(
    () => () => {
      generation.current += 1;
      controller.current?.abort();
    },
    [],
  );

  function load(request: PageRequest) {
    controller.current?.abort();
    const abort = new AbortController();
    controller.current = abort;
    const requestId = ++generation.current;
    // Paging a pinned history must not claim that newer records were read.
    const readRevision =
      request.through === undefined ? revision : (read.revision ?? revision);
    requested.current = request;
    setRead((before) => ({ ...before, status: "reading" }));
    fetchPage(request, abort.signal)
      .then((data) => {
        if (generation.current !== requestId) return;
        controller.current = null;
        setRead({ status: "ready", data, revision: readRevision });
      })
      .catch(() => {
        if (generation.current !== requestId) return;
        controller.current = null;
        setRead((before) => ({ ...before, status: "failed" }));
      });
  }

  function toggle() {
    if (open) {
      generation.current += 1;
      controller.current?.abort();
      controller.current = null;
      if (read.status === "reading") {
        setRead((before) => ({
          ...before,
          status: before.data === null ? "idle" : "ready",
        }));
      }
    } else if (
      read.data === null ||
      read.revision !== revision ||
      read.status === "failed"
    ) {
      load({ offset: 0, limit: RECORD_PAGE_SIZE });
    }
    setOpen(!open);
  }

  const data = read.data;
  const busy = read.status === "reading";
  const page = data === null ? 1 : Math.floor(data.offset / data.limit) + 1;
  const totalPages =
    data === null ? 1 : Math.max(1, Math.ceil(data.total / data.limit));
  const stale = data !== null && read.revision !== revision;

  return (
    <div className="record-table-shell">
      <RecordTableControls
        label={label}
        summary={summary}
        open={open}
        controls={controls}
        onToggle={toggle}
      />
      <div id={controls} hidden={!open}>
        {open && (
          <div className="record-page-status">
            {busy && (
              <p className="micro" role="status">
                Reading {label}…
              </p>
            )}
            {read.status === "failed" && (
              <p className="micro" role="alert">
                The {label} could not be read.
              </p>
            )}
            {stale && !busy && (
              <p className="micro" role="status">
                The record has changed. Refresh to read the latest page.
              </p>
            )}
            {read.status === "failed" ? (
              <Button
                size="xs"
                variant="outline"
                aria-label={`Retry ${label}`}
                onClick={() => load(requested.current)}
              >
                Retry
              </Button>
            ) : (
              data !== null && (
                <Button
                  size="xs"
                  variant="outline"
                  aria-label={`Refresh ${label}`}
                  disabled={busy}
                  onClick={() => load({ offset: 0, limit: RECORD_PAGE_SIZE })}
                >
                  Refresh
                </Button>
              )
            )}
          </div>
        )}
        <div hidden={read.status !== "ready"} aria-busy={busy}>
          {data !== null && children(data)}
        </div>
        {data !== null && (
          <RecordTablePager
            label={pagerLabel}
            page={page}
            totalPages={totalPages}
            disabled={busy}
            onPage={(next) =>
              load({
                offset: (next - 1) * RECORD_PAGE_SIZE,
                limit: RECORD_PAGE_SIZE,
                ...(data.through === null ? {} : { through: data.through }),
              })
            }
          />
        )}
      </div>
    </div>
  );
}
