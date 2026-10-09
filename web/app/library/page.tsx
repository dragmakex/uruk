import type { Metadata } from "next";
import Link from "next/link";
import { ApiDown } from "@/components/ApiDown";
import { ApiFailure, fetchLibrary } from "@/lib/api";
import { shortRunId } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";
import type { LibraryFilter, SourceView } from "@/lib/types";

export const metadata: Metadata = { title: "Library" };
export const dynamic = "force-dynamic";

/** The filter values valid on the API; the form offers exactly these. */
const ACCESS_LEVELS = [
  "full_text",
  "abstract_only",
  "metadata_only",
  "unavailable",
] as const;
const ORIGIN_KINDS = ["local_file", "url", "dataset", "code", "supplied"] as const;

function originText(source: SourceView): string {
  const kind = source.origin.kind.replace(/_/g, " ");
  return source.origin.at !== null ? `${kind}: ${source.origin.at}` : kind;
}

function cleanFilter(params: {
  access?: string;
  origin?: string;
  q?: string;
}): LibraryFilter {
  const filter: LibraryFilter = {};
  if (params.access) filter.access = params.access;
  if (params.origin) filter.origin = params.origin;
  if (params.q?.trim()) filter.q = params.q.trim();
  return filter;
}

/**
 * Every source recorded across this browser's runs, with the provenance
 * facts the engine stores: how it was obtained, how much was readable,
 * and the content hash pinning exactly what was read. Ownerless runs
 * (from builds that predate the web-only product) belong to no browser
 * workspace, so their sources do not appear here. The metadata filters
 * are applied by the API itself.
 */
export default async function LibraryPage({
  searchParams,
}: {
  searchParams: Promise<{ access?: string; origin?: string; q?: string }>;
}) {
  const params = await searchParams;
  const filter = cleanFilter(params);
  const filtered = Object.keys(filter).length > 0;

  let sources: SourceView[];
  try {
    sources = await fetchLibrary(await identityCookieHeader(), filter);
  } catch (e) {
    const detail =
      e instanceof ApiFailure ? `kind: ${e.kind}` : "unexpected failure";
    return (
      <div className="canvas">
        <div className="page-head">
          <h1>Library</h1>
        </div>
        <ApiDown detail={detail} />
      </div>
    );
  }

  return (
    <div className="canvas">
      <div className="page-head">
        <h1>Library</h1>
        <span className="micro">
          {sources.length} source{sources.length === 1 ? "" : "s"}
          {filtered ? " matching" : " across this browser's runs"}
        </span>
        <Link href="/library/search" className="btn btn-outline">
          Search passages
        </Link>
      </div>

      <form method="get" className="filter-bar" aria-label="Filter sources">
        <div className="filter-field">
          <label htmlFor="filter-access">Access</label>
          <select id="filter-access" name="access" defaultValue={params.access ?? ""}>
            <option value="">any</option>
            {ACCESS_LEVELS.map((level) => (
              <option key={level} value={level}>
                {level.replace(/_/g, " ")}
              </option>
            ))}
          </select>
        </div>
        <div className="filter-field">
          <label htmlFor="filter-origin">Origin</label>
          <select id="filter-origin" name="origin" defaultValue={params.origin ?? ""}>
            <option value="">any</option>
            {ORIGIN_KINDS.map((kind) => (
              <option key={kind} value={kind}>
                {kind.replace(/_/g, " ")}
              </option>
            ))}
          </select>
        </div>
        <div className="filter-field filter-grow">
          <label htmlFor="filter-q">Metadata</label>
          <input
            id="filter-q"
            name="q"
            type="text"
            defaultValue={params.q ?? ""}
            placeholder="title, authors, identifier"
          />
        </div>
        <button type="submit" className="btn btn-outline">
          Filter
        </button>
      </form>

      {sources.length === 0 ? (
        filtered ? (
          <div className="empty-state">
            <h2>No sources match these filters</h2>
            <p>
              The filters are applied to the recorded metadata exactly;
              loosen them to see every source again.
            </p>
            <Link href="/library" className="btn btn-outline">
              Clear filters
            </Link>
          </div>
        ) : (
          <div className="empty-state">
            <h2>No sources in this browser&apos;s runs</h2>
            <p>
              Sources are recorded when a run ingests files or URLs or
              acquires literature. Start a run with uploads, URLs, or
              literature search to populate this library.
            </p>
            <Link href="/runs" className="btn btn-outline">
              View runs
            </Link>
          </div>
        )
      ) : (
        <div className="table-wrap">
          <table className="data-table">
            <thead>
              <tr>
                <th scope="col">Source</th>
                <th scope="col">Origin</th>
                <th scope="col">Access</th>
                <th scope="col">Run</th>
                <th scope="col">Retrieved</th>
                <th scope="col">Hash</th>
              </tr>
            </thead>
            <tbody>
              {sources.map((source) => (
                <tr key={source.id}>
                  <td>
                    <Link
                      href={`/runs/${encodeURIComponent(source.run_id)}/sources/${encodeURIComponent(source.id)}`}
                    >
                      <strong>{source.title ?? "untitled"}</strong>
                    </Link>
                    {source.authors !== null && (
                      <>
                        <br />
                        <span className="micro">
                          {source.authors}
                          {source.date !== null ? ` (${source.date})` : ""}
                        </span>
                      </>
                    )}
                    {source.access_limitations !== null && (
                      <>
                        <br />
                        <span className="micro">{source.access_limitations}</span>
                      </>
                    )}
                  </td>
                  <td className="micro-ink">{originText(source)}</td>
                  <td className="micro-ink">{source.access.replace(/_/g, " ")}</td>
                  <td>
                    <Link
                      href={`/runs/${encodeURIComponent(source.run_id)}`}
                      className="micro-ink"
                    >
                      {shortRunId(source.run_id)}
                    </Link>
                  </td>
                  <td className="micro-ink">
                    {source.retrieved_at.slice(0, 10)}
                  </td>
                  <td className="micro-ink">{source.content_hash.slice(0, 12)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
