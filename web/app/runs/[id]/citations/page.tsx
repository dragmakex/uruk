import type { Metadata } from "next";
import Link from "next/link";
import { ApiDown } from "@/components/ApiDown";
import { ApiFailure, fetchCitations } from "@/lib/api";
import { shortRunId } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";
import type { CitationView } from "@/lib/types";

export const metadata: Metadata = { title: "Citations" };
export const dynamic = "force-dynamic";

function originLabel(origin: CitationView["origin"]): string {
  if (origin.kind === "deliverable_claim") {
    return `deliverable claim · ${origin.basis.replace(/_/g, " ")}`;
  }
  return "review observation";
}

/**
 * Every persisted citation of the run with its honest resolution. A
 * citation is shown exactly as the engine resolved it: the verified bytes
 * of a span, a coarse locator on a recorded source, or an explicit
 * unresolved with its reason — never an approximation.
 */
export default async function CitationsPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;

  let citations: CitationView[];
  try {
    citations = await fetchCitations(id, await identityCookieHeader());
  } catch (e) {
    if (e instanceof ApiFailure && e.kind === "not_found") {
      return (
        <div className="canvas canvas-narrow">
          <div className="empty-state">
            <h1>Run not found</h1>
            <p>
              No run with id {id} is available to this browser. Runs belong
              to the browser that started them.
            </p>
            <Link href="/runs" className="btn btn-outline">
              Back to runs
            </Link>
          </div>
        </div>
      );
    }
    const detail =
      e instanceof ApiFailure ? `kind: ${e.kind}` : "unexpected failure";
    return (
      <div className="canvas canvas-narrow">
        <div className="page-head">
          <h1>Citations</h1>
        </div>
        <ApiDown detail={detail} />
      </div>
    );
  }

  return (
    <div className="canvas canvas-narrow">
      <div className="page-head">
        <h1>Citations, run {shortRunId(id)}</h1>
        <Link
          href={`/runs/${encodeURIComponent(id)}`}
          className="btn btn-outline"
        >
          Back to the run
        </Link>
      </div>

      {citations.length === 0 ? (
        <div className="empty-state">
          <h2>No citations recorded</h2>
          <p>
            Citations appear once the run has produced a deliverable with
            grounded claims or reviews with source observations.
          </p>
        </div>
      ) : (
        <ol className="cite-list">
          {citations.map((citation, index) => (
            <li key={index} className="cite-item">
              <div className="hit-head">
                <span className="micro">{originLabel(citation.origin)}</span>
              </div>
              <p className="cite-claim">{citation.claim}</p>
              <p className="micro-ink">
                <Link
                  href={`/runs/${encodeURIComponent(id)}/sources/${encodeURIComponent(citation.source_id)}`}
                >
                  {citation.source_title ?? citation.source_id}
                </Link>
                {citation.locator !== null && (
                  <>
                    {" · "}
                    <span>{citation.locator}</span>
                  </>
                )}
              </p>
              {citation.resolution.kind === "span" && (
                <blockquote className="cite-quote">
                  <p>{citation.resolution.text}</p>
                  <footer className="micro">
                    verified bytes {citation.resolution.start}–
                    {citation.resolution.end}
                    {citation.resolution.truncated && ", excerpt truncated"}
                  </footer>
                </blockquote>
              )}
              {citation.resolution.kind === "source_locator" && (
                <p className="micro">
                  resolves to the recorded source at this locator; no exact
                  byte span was cited
                </p>
              )}
              {citation.resolution.kind === "unresolved" && (
                <p className="cite-unresolved">{citation.resolution.reason}</p>
              )}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
