import type { AgentPanel, Stats } from "@/lib/types";
import { ROLES, roleLabel } from "@/lib/roles";

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
  if (panel.counts.waiting_for_human > 0) {
    const n = panel.counts.waiting_for_human;
    return `${n} task${n === 1 ? "" : "s"} waiting for your approval.`;
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
 * One agent box of the topology: filled header while thinking, dashed
 * border while idle, with real task facts in the body. State is always a
 * text label, never color alone.
 */
export function AgentCard({
  panel,
  stats,
}: {
  panel: AgentPanel;
  stats: Stats;
}) {
  const meta = ROLES[panel.role];
  const failed = panel.last_finished?.state === "failed" ? panel.last_finished : null;

  return (
    <section
      className={`agent is-${panel.state}`}
      data-role={panel.role}
      aria-label={`${roleLabel(panel.role)}: ${panel.state}`}
    >
      <header className="agent-head">
        <h3 className="agent-role">{roleLabel(panel.role)}</h3>
        <span className="agent-state">{panel.state}</span>
      </header>
      <div className="agent-body">
        <p>{activityText(panel)}</p>
        {failed !== null && failed.error !== null && (
          <p className="agent-error">
            {panel.counts.failed} failed: {failed.error}
          </p>
        )}
        {panel.assignments !== null && panel.assignments.length > 0 && (
          <table className="assignments">
            <caption>Assignments</caption>
            <tbody>
              {panel.assignments.map((a) => (
                <tr key={a.role}>
                  <td>{roleLabel(a.role)}</td>
                  <td>
                    {a.open > 0
                      ? `${a.open} open${a.running > 0 ? `, ${a.running} running` : ""}`
                      : a.latest_strategy ?? "none"}
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
      </footer>
    </section>
  );
}
