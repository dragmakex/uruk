import type { Metadata } from "next";
import { ApiDown } from "@/components/ApiDown";
import { Button } from "@/components/Button";
import { ApiFailure, fetchTaskReasoning } from "@/lib/api";
import { formatModelIdentity } from "@/lib/format";
import { identityCookieHeader } from "@/lib/server-identity";

export const metadata: Metadata = { title: "Agent reasoning trace" };
export const dynamic = "force-dynamic";

function ReasoningBackLink({ runId }: { runId: string }) {
  return (
    <Button
      href={`/runs/${encodeURIComponent(runId)}`}
      variant="outline"
      size="sm"
      className="reasoning-back"
      aria-label="Back to run"
      title="Back to run"
    >
      <span aria-hidden="true">←</span>
    </Button>
  );
}

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
          <ReasoningBackLink runId={id} />
          <div className="empty-state">
            <h1>Reasoning trace not found</h1>
            <p>
              No such task belongs to a run this browser can reach.
            </p>
          </div>
        </div>
      );
    }
    return (
      <div className="canvas canvas-narrow">
        <ReasoningBackLink runId={id} />
        <div className="page-head"><h1>Agent reasoning trace</h1></div>
        <ApiDown
          detail={error instanceof ApiFailure ? `kind: ${error.kind}` : "unexpected failure"}
        />
      </div>
    );
  }

  return (
    <div className="canvas canvas-narrow">
      <ReasoningBackLink runId={id} />
      <div className="page-head">
        <div>
          <p className="micro">{detail.task.role.replace(/_/g, " ")} · {detail.task.state}</p>
          <h1>{detail.task.strategy}</h1>
        </div>
      </div>

      <aside className="reasoning-note">
        <strong>What this shows.</strong> These are the verbatim reasoning traces
        explicitly returned by the configured model provider as each model request
        streams or completes. Urukai does not infer or claim access to hidden model state,
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
        <ol className="thinking-chat" aria-label="Agent thinking messages">
          {detail.reasoning.map((trace, index) => (
            <li key={`${trace.started_at}-${index}`} className="thinking-message">
              <div className="thinking-message__avatar" aria-hidden="true">
                {detail.task.role.slice(0, 1).toUpperCase()}
              </div>
              <article className="thinking-message__bubble">
                <header>
                  <strong>Thinking {index + 1}</strong>
                  <span>
                    {trace.model === null ? "model not reported" : formatModelIdentity(trace.model)}
                  </span>
                </header>
                <pre>{trace.content}</pre>
                <footer>
                  <time dateTime={trace.started_at}>{trace.started_at}</time>
                  {trace.streaming ? (
                    <span className="thinking-dots" aria-label="Receiving more thinking">
                      <i />
                      <i />
                      <i />
                    </span>
                  ) : null}
                </footer>
              </article>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
