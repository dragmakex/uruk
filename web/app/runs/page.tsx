import type { Metadata } from "next";
import Link from "next/link";
import { ApiDown } from "@/components/ApiDown";
import { ApiFailure, fetchRuns } from "@/lib/api";
import { shortRunId, stateLabel } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";
import type { RunOverview } from "@/lib/types";

export const metadata: Metadata = { title: "Runs" };
export const dynamic = "force-dynamic";

function RunCard({ run }: { run: RunOverview }) {
  const created = run.created_at.slice(0, 16).replace("T", " ");
  return (
    <Link href={`/runs/${encodeURIComponent(run.id)}`} className="run-card">
      <div className="run-card-top">
        <span className="micro">
          run {shortRunId(run.id)}, {created} UTC
        </span>
        <span className="micro-ink">{stateLabel(run.state)}</span>
      </div>
      <p className="run-card-question" style={{ margin: 0 }}>
        {run.question}
      </p>
      <p className="micro" style={{ marginTop: 6, marginBottom: 0 }}>
        {run.mode}
        {run.stop_condition ? `, ${run.stop_condition.replace(/_/g, " ")}` : ""}
        {run.iterations > 0
          ? `, ${run.iterations} round${run.iterations === 1 ? "" : "s"}`
          : ""}
      </p>
    </Link>
  );
}

export default async function RunsPage() {
  let runs: RunOverview[];
  try {
    runs = await fetchRuns(await identityCookieHeader());
  } catch (e) {
    const detail =
      e instanceof ApiFailure ? `kind: ${e.kind}` : "unexpected failure";
    return (
      <div className="canvas canvas-narrow">
        <div className="page-head">
          <h1>Runs</h1>
        </div>
        <ApiDown detail={detail} />
      </div>
    );
  }

  return (
    <div className="canvas canvas-narrow">
      <div className="page-head">
        <h1>Runs</h1>
        <Link href="/runs/new" className="btn">
          Start research
        </Link>
      </div>
      {runs.length === 0 ? (
        <div className="empty-state">
          <h2>No runs in this browser yet</h2>
          <p>
            Specify a research goal and Uruk plans, drafts, reviews, and
            ranks candidate answers against it. Runs stay available to this
            browser through an anonymous cookie; clearing site data loses
            access to them.
          </p>
          <Link href="/runs/new" className="btn">
            Start research
          </Link>
        </div>
      ) : (
        <>
          <div>
            {runs.map((run) => (
              <RunCard key={run.id} run={run} />
            ))}
          </div>
          <p className="micro" style={{ marginTop: 12 }}>
            Runs started in this browser. Access rests on an anonymous
            cookie: clearing site data loses it, and other browsers cannot
            see these runs.
          </p>
        </>
      )}
    </div>
  );
}
