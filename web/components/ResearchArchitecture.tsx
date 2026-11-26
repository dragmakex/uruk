import { useMemo } from "react";
import type { RunSnapshot } from "@/lib/types";
import type { WorkMove } from "@/lib/handoff";
import { AgentCard } from "./AgentCard";
import { Topology } from "./Topology";
import "./research-architecture.css";

const STOP_LABELS: Record<string, string> = {
  deliverable_satisfied: "Deliverable satisfied",
  rounds_completed: "All rounds completed",
  budget_exhausted: "Budget reached",
  work_exhausted: "No permitted work remains",
  progress_stalled: "Progress stalled",
  cancelled: "Researcher stops the run",
  start_incomplete: "Start did not complete",
};

function BriefList({
  label,
  values,
}: {
  label: string;
  values: readonly string[];
}) {
  return (
    <div className="architecture-list">
      <p className="micro-ink">{label}</p>
      {values.length > 0 ? (
        <ul>
          {values.map((value, index) => (
            <li key={index}>{value}</li>
          ))}
        </ul>
      ) : (
        <p className="architecture-muted">None specified</p>
      )}
    </div>
  );
}

function PlanSummary({ snapshot }: { snapshot: RunSnapshot }) {
  const { goal, plan, run, usage } = snapshot;
  return (
    <div className="supervisor-plan">
      <p className="micro-ink">Plan &amp; compute</p>
      <dl className="architecture-facts">
        <div>
          <dt>Model calls</dt>
          <dd>
            {usage.model_calls} / {goal.budget.max_model_calls}
          </dd>
        </div>
        <div>
          <dt>Completed rounds</dt>
          <dd>
            {run.iterations} / {goal.budget.max_iterations}
          </dd>
        </div>
      </dl>
      {plan === null ? (
        <p className="architecture-muted">No plan recorded yet.</p>
      ) : (
        <details className="architecture-details">
          <summary>Plan details</summary>
          <p>{plan.rationale}</p>
          <BriefList label="Methods" values={plan.methods} />
          <BriefList
            label="Stopping conditions"
            values={plan.stopping_conditions.map(
              (condition) =>
                STOP_LABELS[condition] ?? condition.replaceAll("_", " "),
            )}
          />
        </details>
      )}
    </div>
  );
}

/** Architecture boundaries projected from the existing snapshot; no extra reads. */
export function ResearchArchitecture({
  snapshot,
  onMove,
}: {
  snapshot: RunSnapshot;
  onMove: (move: WorkMove) => void;
}) {
  const { goal, stats, agents, run, usage } = snapshot;
  const supporting = agents.filter(
    (agent) => agent.role === "search" || agent.role === "tool",
  );
  const research = useMemo(
    () =>
      agents.filter(
        (agent) => agent.role !== "search" && agent.role !== "tool",
      ),
    [agents],
  );
  return (
    <section className="research-system" aria-label="Research system">
      <header className="architecture-heading">
        <div>
          <h2>Research system</h2>
          <p>One supervisor. Independent workers. Shared context.</p>
        </div>
        <span className="micro">
          {goal.mode === "campaign" ? "Iterative campaign" : "Bounded task"}
        </span>
      </header>

      <div className="research-map">
        <div className="scientist-column">
          <section
            className="architecture-panel scientist-input"
            aria-label="Scientist input"
          >
            <p className="architecture-eyebrow micro">Scientist → supervisor</p>
            <h3>Research brief</h3>
            <p className="research-question">{goal.question}</p>
            <p className="architecture-muted">
              Goal and criteria are fixed for this run.
            </p>
            <BriefList
              label="Evaluation criteria"
              values={goal.rubric.attributes}
            />
            <BriefList label="Constraints" values={goal.rubric.constraints} />
            <details className="architecture-details">
              <summary>Brief details</summary>
              <BriefList label="Preferences" values={goal.rubric.preferences} />
              <BriefList
                label="Acceptance criteria"
                values={goal.rubric.acceptance}
              />
              <BriefList label="Requested outputs" values={goal.deliverables} />
            </details>
          </section>
          <section
            className="architecture-panel research-outputs"
            aria-label="Scientist outputs"
          >
            <p className="architecture-eyebrow micro">Back to the scientist</p>
            <h3>Research outputs</h3>
            <a href="#run-overview">
              Leading proposals <span aria-hidden="true">↗</span>
            </a>
            <a href="#run-candidates">
              All candidates <span aria-hidden="true">↓</span>
            </a>
            <p className="architecture-muted">
              Proposals need independent validation; Elo is not scientific
              confidence.
            </p>
          </section>
        </div>

        <div className="research-engine">
          <Topology
            key={run.id}
            runId={run.id}
            agents={research}
            stats={stats}
            onMove={onMove}
            supervisorDetails={<PlanSummary snapshot={snapshot} />}
          />
          <p className="architecture-flow-note micro">
            The supervisor allocates work as results arrive, not in a fixed
            six-step sequence.
          </p>
        </div>

        <section
          className="architecture-panel research-tools"
          aria-label="Tools and retrieval"
        >
          <p className="architecture-eyebrow micro">External resources</p>
          <h3>Tools &amp; retrieval</h3>
          <dl className="architecture-facts">
            <div>
              <dt>Network retrieval</dt>
              <dd>
                {goal.permissions.network ? "Permitted" : "Not permitted"}
              </dd>
            </div>
            <div>
              <dt>Local execution</dt>
              <dd>
                {goal.permissions.execute ? "Permitted" : "Not permitted"}
              </dd>
            </div>
            <div>
              <dt>Recorded searches</dt>
              <dd>{stats.searches}</dd>
            </div>
            <div>
              <dt>Discovered works</dt>
              <dd>{stats.works}</dd>
            </div>
            <div>
              <dt>Tool executions</dt>
              <dd>{usage.tool_executions}</dd>
            </div>
          </dl>
          <p className="architecture-muted">
            Permissions are not a connection or availability check.
          </p>
          <details className="architecture-details">
            <summary>Allowed tools</summary>
            {goal.permissions.allowed_tools.length > 0 ? (
              <ul className="allowed-tools">
                {goal.permissions.allowed_tools.map((tool, index) => (
                  <li key={index}>
                    <code>{tool}</code>
                  </li>
                ))}
              </ul>
            ) : (
              <p>No tools permitted.</p>
            )}
          </details>
          {supporting.length > 0 && (
            <p className="resource-workers-label micro-ink">
              Supervised resource workers
            </p>
          )}
          {supporting.map((panel) => (
            <AgentCard
              key={panel.role}
              runId={run.id}
              panel={panel}
              stats={stats}
            />
          ))}
        </section>

        <section className="research-memory" aria-label="Shared context memory">
          <header>
            <h3>Shared context memory</h3>
            <p className="micro">Persisted for the supervisor and workers</p>
          </header>
          <dl className="memory-records">
            <div>
              <dt>{stats.items === 1 ? "Candidate" : "Candidates"}</dt>
              <dd>
                <a
                  href="#run-candidates"
                  aria-label={`${stats.items} ${stats.items === 1 ? "candidate" : "candidates"} in shared memory`}
                >
                  {stats.items}
                </a>
              </dd>
            </div>
            <div>
              <dt>{stats.reviews === 1 ? "Review" : "Reviews"}</dt>
              <dd>{stats.reviews}</dd>
            </div>
            <div>
              <dt>{stats.matches === 1 ? "Elo match" : "Elo matches"}</dt>
              <dd>
                <a
                  href="#run-matches"
                  aria-label={`${stats.matches} ${stats.matches === 1 ? "match" : "matches"} in shared memory`}
                >
                  {stats.matches}
                </a>
              </dd>
            </div>
            <div>
              <dt>{stats.clusters === 1 ? "Cluster" : "Clusters"}</dt>
              <dd>{stats.clusters}</dd>
            </div>
            <div>
              <dt>{stats.decisions === 1 ? "Decision" : "Decisions"}</dt>
              <dd>
                <a
                  href="#run-decisions"
                  aria-label={`${stats.decisions} ${stats.decisions === 1 ? "decision" : "decisions"} in shared memory`}
                >
                  {stats.decisions}
                </a>
              </dd>
            </div>
            <div>
              <dt>{stats.sources === 1 ? "Source" : "Sources"}</dt>
              <dd>{stats.sources}</dd>
            </div>
          </dl>
          <a className="memory-library" href="/library">
            Open source library across your runs{" "}
            <span aria-hidden="true">↗</span>
          </a>
        </section>
      </div>
      <details className="legend-fold">
        <summary>Key</summary>
        <div className="legend-line">
          <span className="micro">Filled header: thinking</span>
          <span className="micro">Dashed box: idle</span>
          <span className="micro">
            Connections: supervision, not execution order
          </span>
          <span className="micro">Moving dot: an observed worker start</span>
        </div>
      </details>
    </section>
  );
}
