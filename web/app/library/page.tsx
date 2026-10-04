import type { Metadata } from "next";
import Link from "next/link";
import { ApiDown } from "@/components/ApiDown";
import { ApiFailure, fetchLibrary } from "@/lib/api";
import { shortRunId } from "@/lib/format";
import type { SourceView } from "@/lib/types";

export const metadata: Metadata = { title: "Library" };
export const dynamic = "force-dynamic";

function originText(source: SourceView): string {
  const kind = source.origin.kind.replace(/_/g, " ");
  return source.origin.at !== null ? `${kind}: ${source.origin.at}` : kind;
}

/**
 * Every source recorded across runs, with the provenance facts the engine
 * stores: how it was obtained, how much was readable, and the content hash
 * pinning exactly what was read.
 */
export default async function LibraryPage() {
  let sources: SourceView[];
  try {
    sources = await fetchLibrary();
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
          {sources.length} source{sources.length === 1 ? "" : "s"} across all
          runs
        </span>
      </div>

      {sources.length === 0 ? (
        <div className="empty-state">
          <h2>No sources yet</h2>
          <p>
            Sources are recorded when a run ingests supplied files or URLs
            (CLI `--input`) or acquires literature (`--search`). Web-started
            runs grant no inputs, so this library fills from CLI runs.
          </p>
          <Link href="/runs" className="btn btn-outline">
            View runs
          </Link>
        </div>
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
                    <strong>{source.title ?? "untitled"}</strong>
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
