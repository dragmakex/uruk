import type { Metadata } from "next";
import Link from "next/link";
import { LiveRun } from "@/components/LiveRun";
import { ApiFailure, fetchSnapshot } from "@/lib/api";
import type { RunSnapshot } from "@/lib/types";

export const metadata: Metadata = { title: "Run" };
export const dynamic = "force-dynamic";

/**
 * The live run page. The first snapshot is fetched on the server so the
 * page renders complete; the client component then follows the SSE stream.
 */
export default async function RunPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;

  let initial: RunSnapshot | null = null;
  try {
    initial = await fetchSnapshot(id);
  } catch (e) {
    if (e instanceof ApiFailure && e.kind === "not_found") {
      return (
        <div className="canvas canvas-narrow">
          <div className="empty-state">
            <h1>Run not found</h1>
            <p>No run with id {id} exists in this project.</p>
            <Link href="/runs" className="btn btn-outline">
              Back to runs
            </Link>
          </div>
        </div>
      );
    }
    // API down or invalid payload: the client stream keeps trying and the
    // component shows the connection state honestly.
    initial = null;
  }

  return <LiveRun runId={id} initial={initial} />;
}
