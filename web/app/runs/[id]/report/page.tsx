import { Button } from "@/components/Button";
import type { Metadata } from "next";
import { ApiDown } from "@/components/ApiDown";
import { ReportMarkdown } from "@/components/ReportMarkdown";
import { ApiFailure, fetchReport } from "@/lib/api";
import { shortRunId } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";
import type { ReportView } from "@/lib/types";

export const metadata: Metadata = { title: "Report" };
export const dynamic = "force-dynamic";

/** Manifest facts worth a cell, when the manifest carries them. */
function manifestFacts(
  manifest: Record<string, unknown>,
): Array<{ label: string; value: string }> {
  const facts: Array<{ label: string; value: string }> = [];
  if (typeof manifest.state === "string") {
    facts.push({ label: "state", value: manifest.state });
  }
  if (typeof manifest.partial === "boolean") {
    facts.push({ label: "partial", value: manifest.partial ? "yes" : "no" });
  }
  const usage = manifest.usage;
  if (typeof usage === "object" && usage !== null) {
    const u = usage as Record<string, unknown>;
    if (typeof u.model_calls === "number") {
      facts.push({ label: "model calls", value: String(u.model_calls) });
    }
    if (typeof u.tokens === "number") {
      facts.push({ label: "tokens", value: u.tokens.toLocaleString("en-US") });
    }
    facts.push({
      label: "cost",
      value: typeof u.cost_usd === "number" ? `$${u.cost_usd.toFixed(4)}` : "unknown",
    });
  }
  const counts = manifest.counts;
  if (typeof counts === "object" && counts !== null) {
    const c = counts as Record<string, unknown>;
    for (const key of ["items", "sources", "reviews", "matches", "evidence"]) {
      if (typeof c[key] === "number") {
        facts.push({ label: key, value: String(c[key]) });
      }
    }
  }
  return facts;
}

/**
 * The exported report, rendered for reading. The report is the
 * scientific record, so the canonical bytes remain available unmodified:
 * Download Markdown serves `REPORT.md` verbatim and Download PDF serves
 * its deterministic PDF twin, both straight from the engine. The
 * rendering below is a readable view of those bytes, never a substitute
 * for them (web/README.md documents this choice).
 */
export default async function ReportPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;

  let report: ReportView;
  try {
    report = await fetchReport(id, await identityCookieHeader());
  } catch (e) {
    if (e instanceof ApiFailure && e.kind === "not_found") {
      return (
        <div className="canvas canvas-narrow">
          <div className="empty-state">
            <h1>No report yet</h1>
            <p>
              A report is exported when the run finishes (or is stopped). If
              the run is still live, watch it instead.
            </p>
            <Button href={`/runs/${encodeURIComponent(id)}`} variant="outline" size="sm">
              Back to the run
            </Button>
          </div>
        </div>
      );
    }
    const detail =
      e instanceof ApiFailure ? `kind: ${e.kind}` : "unexpected failure";
    return (
      <div className="canvas canvas-narrow">
        <div className="page-head">
          <h1>Report</h1>
        </div>
        <ApiDown detail={detail} />
      </div>
    );
  }

  const facts = report.manifest ? manifestFacts(report.manifest) : [];

  return (
    <div className="canvas canvas-narrow">
      <div className="page-head">
        <h1>Report, run {shortRunId(report.run_id)}</h1>
        <div className="report-actions">
          <Button
            href={`/runs/${encodeURIComponent(report.run_id)}/citations`}
            variant="outline" size="sm"
          >
            Inspect citations
          </Button>
          <Button
            href={`/api/runs/${encodeURIComponent(report.run_id)}/report.pdf`}
            variant="outline" size="sm"
          >
            Download PDF
          </Button>
          <Button
            href={`/api/runs/${encodeURIComponent(report.run_id)}/report.md`}
            variant="outline" size="sm"
          >
            Download Markdown
          </Button>
          <Button
            href={`/runs/${encodeURIComponent(report.run_id)}`}
            variant="outline" size="sm"
          >
            Back to the run
          </Button>
        </div>
      </div>

      {facts.length > 0 && (
        <dl className="manifest-grid">
          {facts.map((fact) => (
            <div className="manifest-cell" key={fact.label}>
              <dt>{fact.label}</dt>
              <dd>{fact.value}</dd>
            </div>
          ))}
        </dl>
      )}

      <div className="panel">
        <ReportMarkdown markdown={report.markdown} />
      </div>
    </div>
  );
}
