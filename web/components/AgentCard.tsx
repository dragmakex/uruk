import type { AgentPanel, Stats } from "@/lib/types";
import { ROLES, roleLabel } from "@/lib/roles";
import Link from "next/link";

/**
 * One honest sentence about what a panel's tasks are doing right now.
 * Derived strictly from recorded task facts; when there are none, it says
 * so instead of inventing activity.
 */
export function activityText(panel: AgentPanel): string {
  if (panel.current) {
    const attempts =
      panel.current.attempts > 1 ? `, attempt ${panel.current.attempts}` : "";
    return `Running ${panel.current.strategy}${attempts}.`;
  }
  const queued = panel.counts.pending + panel.counts.blocked;
  if (queued > 0) {
    return `${queued} task${queued === 1 ? "" : "s"} queued.`;
  }
  if (panel.last_finished) {
    return `Last task: ${panel.last_finished.strategy}, ${panel.last_finished.state}.`;
  }
  return "No work yet this run.";
}

/** The mono footer stat for a role, from run-wide facts and task counts. */
export function footerStat(panel: AgentPanel, stats: Stats): string {
  const counted = (n: number, singular: string, pluralWord: string) =>
    `${n} ${n === 1 ? singular : pluralWord}`;
  switch (panel.role) {
    case "supervisor":
      return `${stats.workers_busy} of ${stats.workers_total} workers busy`;
    case "generation":
      return counted(stats.items, "item", "items");
    case "reflection":
      return counted(stats.reviews, "review", "reviews");
    case "ranking":
      return counted(stats.matches, "match", "matches");
    case "proximity":
      return counted(stats.clusters, "cluster", "clusters");
    case "meta_review":
      return counted(panel.counts.completed, "summary", "summaries");
    case "search":
      return counted(stats.works, "work", "works");
    default:
      return `${panel.counts.completed} completed`;
  }
}

/**
 * One agent box of the topology. A box with task history is one large link to
 * that task's full provider-supplied reasoning trace.
 */
export function AgentCard({
  runId,
  panel,
  stats,
}: {
  runId: string;
  panel: AgentPanel;
  stats: Stats;
}) {
  const meta = ROLES[panel.role];
  const failed = panel.last_finished?.state === "failed" ? panel.last_finished : null;
  const displayedTask = panel.current?.reasoning_preview
    ? panel.current
    : panel.last_finished?.reasoning_preview
      ? panel.last_finished
      : (panel.current ?? panel.last_finished);
  const detailHref = displayedTask
    ? `/runs/${encodeURIComponent(runId)}/tasks/${encodeURIComponent(displayedTask.id)}/reasoning`
    : null;

  const content = (
    <>
      <header className="agent-head">
        <h3 className="agent-role">{roleLabel(panel.role)}</h3>
        <span className="agent-state">{panel.state}</span>
      </header>
      <div className="agent-body">
        <p>{activityText(panel)}</p>
        {displayedTask !== null && (
          <p className="agent-reasoning-preview">
            <span className="agent-reasoning-label">Reasoning</span>
            <span className="agent-reasoning-copy">
              {displayedTask.reasoning_preview ?? "Waiting for a provider trace…"}
            </span>
          </p>
        )}
        {failed !== null && failed.error !== null && (
          <p className="agent-error">
            {panel.counts.failed} failed: {failed.error}
          </p>
        )}
        {panel.assignments !== null && panel.assignments.length > 0 && (
          <table className="assignments">
            <caption>Assignments</caption>
            <tbody>
              {panel.assignments.map((assignment) => (
                <tr key={assignment.role}>
                  <td>{roleLabel(assignment.role)}</td>
                  <td>
                    {assignment.open > 0
                      ? `${assignment.open} open${assignment.running > 0 ? `, ${assignment.running} running` : ""}`
                      : assignment.latest_strategy ?? "none"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {meta?.feeds ? (
          <p className="flow-note micro">feeds {roleLabel(meta.feeds)}</p>
        ) : null}
      </div>
      <footer className="agent-foot">
        <span className="micro">{footerStat(panel, stats)}</span>
        {displayedTask !== null && (
          <span className="agent-reasoning-link">View full reasoning →</span>
        )}
      </footer>
    </>
  );

  if (detailHref !== null) {
    return (
      <Link
        href={detailHref}
        aria-label={`${roleLabel(panel.role)} — view full reasoning`}
        className={`agent agent-link is-${panel.state}`}
        data-role={panel.role}
      >
        {content}
      </Link>
    );
  }

  return (
    <section
      className={`agent is-${panel.state}`}
      data-role={panel.role}
      aria-label={`${roleLabel(panel.role)}: ${panel.state}`}
    >
      {content}
    </section>
  );
}
