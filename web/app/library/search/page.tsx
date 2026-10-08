import type { Metadata } from "next";
import Link from "next/link";
import { ApiDown } from "@/components/ApiDown";
import { ApiFailure, searchLibraryPassages } from "@/lib/api";
import { shortRunId } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";
import type { LibraryPassageHit } from "@/lib/types";

export const metadata: Metadata = { title: "Passage search" };
export const dynamic = "force-dynamic";

/**
 * Owner-wide passage search: BM25-ranked passages from every source this
 * browser's runs have indexed. Each hit names its source and run and
 * links into the source's own page, where the passage can be read in
 * context.
 */
export default async function LibrarySearchPage({
  searchParams,
}: {
  searchParams: Promise<{ q?: string }>;
}) {
  const { q } = await searchParams;
  const query = q?.trim() ?? "";

  let hits: LibraryPassageHit[] | null = null;
  if (query !== "") {
    try {
      hits = await searchLibraryPassages(query, await identityCookieHeader());
    } catch (e) {
      const detail =
        e instanceof ApiFailure ? `kind: ${e.kind}` : "unexpected failure";
      return (
        <div className="canvas canvas-narrow">
          <div className="page-head">
            <h1>Passage search</h1>
          </div>
          <ApiDown detail={detail} />
        </div>
      );
    }
  }

  return (
    <div className="canvas canvas-narrow">
      <div className="page-head">
        <h1>Passage search</h1>
        <Link href="/library" className="btn btn-outline">
          Back to the library
        </Link>
      </div>

      <form method="get" className="filter-bar" aria-label="Passage search">
        <div className="filter-field filter-grow">
          <label htmlFor="search-q">Search passages</label>
          <input
            id="search-q"
            name="q"
            type="text"
            defaultValue={query}
            placeholder="terms to search across every indexed source"
          />
        </div>
        <button type="submit" className="btn btn-outline">
          Search
        </button>
      </form>

      {hits !== null &&
        (hits.length === 0 ? (
          <div className="empty-state">
            <h2>No passages match</h2>
            <p>
              Only text this browser&apos;s runs have extracted and indexed is
              searchable; a source without full text contributes nothing.
            </p>
          </div>
        ) : (
          <ol className="hit-list">
            {hits.map((hit) => (
              <li key={`${hit.run_id}/${hit.source_id}/${hit.seq}`} className="hit-row">
                <div className="hit-head">
                  <Link
                    href={`/runs/${encodeURIComponent(hit.run_id)}/sources/${encodeURIComponent(hit.source_id)}`}
                  >
                    {hit.source_title ?? hit.source_id}
                  </Link>
                  <span className="micro">
                    run {shortRunId(hit.run_id)} · passage #{hit.seq} · bytes{" "}
                    {hit.byte_start}–{hit.byte_end}
                  </span>
                </div>
                <p className="hit-snippet">{hit.snippet}</p>
              </li>
            ))}
          </ol>
        ))}
    </div>
  );
}
