import type { Metadata } from "next";
import { ApiDown } from "@/components/ApiDown";
import { Button } from "@/components/Button";
import { ApiFailure, fetchTaskReasoning } from "@/lib/api";
import { identityCookieHeader } from "@/lib/server-identity";

export const metadata: Metadata = { title: "Agent reasoning trace" };
export const dynamic = "force-dynamic";

/**
 * Verbatim provider-supplied reasoning for one agent task. The page is
 * intentionally fetched on demand: reasoning can be large and should not
 * inflate the live run snapshot or its SSE refreshes.
 */
export default async function TaskReasoningPage({
  params,
}: {
  params: Promise<{ id: string; taskId: string }>;
}) {
  const { id, taskId } = await params;
  const cookie = await identityCookieHeader();

  let detail;
  try {
    detail = await fetchTaskReasoning(id, taskId, cookie);
  } catch (error) {
    if (error instanceof ApiFailure && error.kind === "not_found") {
      return (
        <div className="canvas canvas-narrow">
          <div className="empty-state">
            <h1>Reasoning trace not found</h1>
            <p>
              No such task belongs to a run this browser can reach.
            </p>
            <Button href={`/runs/${encodeURIComponent(id)}`} variant="outline" size="sm">
              Back to run
            </Button>
          </div>
        </div>
      );
    }
    return (
      <div className="canvas canvas-narrow">
        <div className="page-head"><h1>Agent reasoning trace</h1></div>
        <ApiDown
          detail={error instanceof ApiFailure ? `kind: ${error.kind}` : "unexpected failure"}
        />
      </div>
    );
  }

  return (
    <div className="canvas canvas-narrow">
      <div className="page-head">
        <div>
          <p className="micro">{detail.task.role.replace(/_/g, " ")} · {detail.task.state}</p>
          <h1>{detail.task.strategy}</h1>
        </div>
        <Button href={`/runs/${encodeURIComponent(id)}`} variant="outline" size="sm">
          Back to run
        </Button>
      </div>

      <aside className="reasoning-note">
        <strong>What this shows.</strong> These are the verbatim reasoning traces
        explicitly returned by the configured model provider after each completed
        model request. Uruk does not infer or claim access to hidden model state,
        and a provider that omits <code>reasoning_content</code> leaves no trace.
      </aside>

      {detail.reasoning.length === 0 ? (
        <div className="empty-state">
          <h2>No provider reasoning recorded</h2>
          <p>
            This task may still be waiting for its first model response, or its
            provider/model may not return an explicit reasoning field.
          </p>
        </div>
      ) : (
        <ol className="reasoning-list">
          {detail.reasoning.map((trace, index) => (
            <li key={`${trace.started_at}-${index}`} className="reasoning-entry">
              <header>
                <span>Model call {index + 1}</span>
                <span className="micro">
                  {trace.model ?? "model not reported"} · {trace.started_at}
                </span>
              </header>
              <pre>{trace.content}</pre>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
