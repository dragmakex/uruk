import type { Metadata } from "next";
import Link from "next/link";
import { ApiDown } from "@/components/ApiDown";
import {
  ApiFailure,
  browseSourcePassages,
  fetchSourceDetail,
  searchSourcePassages,
} from "@/lib/api";
import { shortRunId } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";
import type {
  PassagePage,
  SourceDetail,
  SourcePassageHit,
  SourceView,
} from "@/lib/types";

export const metadata: Metadata = { title: "Source" };
export const dynamic = "force-dynamic";

function originText(source: SourceView): string {
  const kind = source.origin.kind.replace(/_/g, " ");
  return source.origin.at !== null ? `${kind}: ${source.origin.at}` : kind;
}

/** Recorded facts worth a cell; everything shown is stored, not derived. */
function sourceFacts(detail: SourceDetail): Array<{ label: string; value: string }> {
  const source = detail.source;
  const facts = [
    { label: "origin", value: originText(source) },
    { label: "access", value: source.access.replace(/_/g, " ") },
    { label: "retrieved", value: source.retrieved_at.slice(0, 10) },
    { label: "content hash", value: source.content_hash },
    { label: "indexed passages", value: String(detail.passage_count) },
  ];
  if (source.identifier !== null) {
    facts.push({ label: "identifier", value: source.identifier });
  }
  if (source.access_limitations !== null) {
    facts.push({ label: "limitations", value: source.access_limitations });
  }
  return facts;
}

/**
 * One recorded source and its indexed text. Without `q` the passages are
 * browsable in artifact order with their byte spans — the same spans
 * `chars N..M` citations use — and with `q` they are searched with BM25
 * inside this source only.
 */
export default async function SourcePage({
  params,
  searchParams,
}: {
  params: Promise<{ id: string; sourceId: string }>;
  searchParams: Promise<{ q?: string; offset?: string }>;
}) {
  const { id, sourceId } = await params;
  const { q, offset: rawOffset } = await searchParams;
  const query = q?.trim() ?? "";
  const cookie = await identityCookieHeader();

  let detail: SourceDetail;
  let hits: SourcePassageHit[] | null = null;
  let page: PassagePage | null = null;
  try {
    detail = await fetchSourceDetail(id, sourceId, cookie);
    if (query !== "") {
      hits = await searchSourcePassages(id, sourceId, query, cookie);
    } else {
      const offset = /^\d+$/.test(rawOffset ?? "")
        ? Number(rawOffset)
        : undefined;
      page = await browseSourcePassages(id, sourceId, { offset }, cookie);
    }
  } catch (e) {
    if (e instanceof ApiFailure && e.kind === "not_found") {
      return (
        <div className="canvas canvas-narrow">
          <div className="empty-state">
            <h1>Source not found</h1>
            <p>
              No such source is recorded in a run this browser can reach.
              Sources belong to the run that recorded them and to the
              browser that started it.
            </p>
            <Link href="/library" className="btn btn-outline">
              Back to the library
            </Link>
          </div>
        </div>
      );
    }
    const detailText =
      e instanceof ApiFailure ? `kind: ${e.kind}` : "unexpected failure";
    return (
      <div className="canvas canvas-narrow">
        <div className="page-head">
          <h1>Source</h1>
        </div>
        <ApiDown detail={detailText} />
      </div>
    );
  }

  const source = detail.source;
  const base = `/runs/${encodeURIComponent(id)}/sources/${encodeURIComponent(sourceId)}`;

  return (
    <div className="canvas canvas-narrow">
      <div className="page-head">
        <h1>{source.title ?? "untitled source"}</h1>
        <Link
          href={`/runs/${encodeURIComponent(id)}`}
          className="btn btn-outline"
        >
          Run {shortRunId(id)}
        </Link>
      </div>

      {source.authors !== null && (
        <p className="micro-ink">
          {source.authors}
          {source.date !== null ? ` (${source.date})` : ""}
        </p>
      )}

      <dl className="manifest-grid">
        {sourceFacts(detail).map((fact) => (
          <div className="manifest-cell" key={fact.label}>
            <dt>{fact.label}</dt>
            <dd className="fact-mono">{fact.value}</dd>
          </div>
        ))}
      </dl>

      <form method="get" className="filter-bar" aria-label="Source search">
        <div className="filter-field filter-grow">
          <label htmlFor="source-q">Search this source</label>
          <input
            id="source-q"
            name="q"
            type="text"
            defaultValue={query}
            placeholder="terms to search in this source's text"
          />
        </div>
        <button type="submit" className="btn btn-outline">
          Search
        </button>
        {query !== "" && (
          <Link href={base} className="btn btn-outline">
            Browse all
          </Link>
        )}
      </form>

      {hits !== null &&
        (hits.length === 0 ? (
          <div className="empty-state">
            <h2>No passages match</h2>
            <p>The search covers only this source&apos;s indexed text.</p>
          </div>
        ) : (
          <ol className="hit-list">
            {hits.map((hit) => (
              <li key={hit.seq} className="hit-row">
                <div className="hit-head">
                  <span className="micro">
                    passage #{hit.seq} · bytes {hit.byte_start}–{hit.byte_end}
                  </span>
                </div>
                <p className="hit-snippet">{hit.snippet}</p>
              </li>
            ))}
          </ol>
        ))}

      {page !== null &&
        (page.passages.length === 0 ? (
          <div className="empty-state">
            <h2>No indexed text</h2>
            <p>
              This source has no extracted-text passages
              {page.total > 0 ? " on this page" : ""}. Only full-text
              sources are chunked and indexed.
            </p>
          </div>
        ) : (
          <>
            <ol className="hit-list">
              {page.passages.map((passage) => (
                <li key={passage.seq} className="hit-row">
                  <div className="hit-head">
                    <span className="micro">
                      passage #{passage.seq} · bytes {passage.byte_start}–
                      {passage.byte_end}
                    </span>
                  </div>
                  <p className="hit-snippet">{passage.text}</p>
                </li>
              ))}
            </ol>
            <div className="pager">
              <span className="micro">
                {page.offset + 1}–{page.offset + page.passages.length} of{" "}
                {page.total}
              </span>
              {page.offset > 0 && (
                <Link
                  href={
                    page.offset - page.limit > 0
                      ? `${base}?offset=${page.offset - page.limit}`
                      : base
                  }
                  className="btn btn-outline"
                >
                  Previous
                </Link>
              )}
              {page.offset + page.passages.length < page.total && (
                <Link
                  href={`${base}?offset=${page.offset + page.limit}`}
                  className="btn btn-outline"
                >
                  Next
                </Link>
              )}
            </div>
          </>
        ))}
    </div>
  );
}
