/**
 * Runtime validation at the API boundary.
 *
 * Every byte from the network goes through these parsers before it reaches a
 * component; nothing silently trusts arbitrary JSON. Parsers throw
 * [`ParseError`] with a path to the offending field so a contract drift
 * between the Rust view models and these mirrors fails loudly and
 * debuggably instead of rendering nonsense.
 */

import type {
  AgentPanel,
  AgentState,
  ApiErrorBody,
  Assignment,
  BudgetUsage,
  BudgetView,
  GoalView,
  ItemCard,
  Leader,
  PlanView,
  ReportView,
  RunMeta,
  RunOverview,
  RunPreview,
  RunSnapshot,
  RunState,
  SourceView,
  Stats,
  TaskCounts,
  TaskFacts,
  UploadView,
} from "./types";
import { RUN_STATES } from "./types";

export class ParseError extends Error {
  constructor(path: string, expected: string, got: unknown) {
    super(`${path}: expected ${expected}, got ${describe(got)}`);
    this.name = "ParseError";
  }
}

function describe(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value;
}

function record(value: unknown, path: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new ParseError(path, "object", value);
  }
  return value as Record<string, unknown>;
}

function str(value: unknown, path: string): string {
  if (typeof value !== "string") throw new ParseError(path, "string", value);
  return value;
}

function strOrNull(value: unknown, path: string): string | null {
  if (value === null || value === undefined) return null;
  return str(value, path);
}

function num(value: unknown, path: string): number {
  if (typeof value !== "number" || Number.isNaN(value)) {
    throw new ParseError(path, "number", value);
  }
  return value;
}

function numOrNull(value: unknown, path: string): number | null {
  if (value === null || value === undefined) return null;
  return num(value, path);
}

function bool(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") throw new ParseError(path, "boolean", value);
  return value;
}

function array(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) throw new ParseError(path, "array", value);
  return value;
}

function strArray(value: unknown, path: string): string[] {
  return array(value, path).map((v, i) => str(v, `${path}[${i}]`));
}

function runState(value: unknown, path: string): RunState {
  const s = str(value, path);
  if (!(RUN_STATES as readonly string[]).includes(s)) {
    throw new ParseError(path, `one of ${RUN_STATES.join(", ")}`, s);
  }
  return s as RunState;
}

function agentState(value: unknown, path: string): AgentState {
  const s = str(value, path);
  if (s !== "thinking" && s !== "waiting" && s !== "idle") {
    throw new ParseError(path, "thinking | waiting | idle", s);
  }
  return s;
}

function parseRunMeta(value: unknown, path: string): RunMeta {
  const r = record(value, path);
  return {
    id: str(r.id, `${path}.id`),
    state: runState(r.state, `${path}.state`),
    stop_condition: strOrNull(r.stop_condition, `${path}.stop_condition`),
    iterations: num(r.iterations, `${path}.iterations`),
    paused_ms: num(r.paused_ms, `${path}.paused_ms`),
    created_at: str(r.created_at, `${path}.created_at`),
    updated_at: str(r.updated_at, `${path}.updated_at`),
  };
}

function parseBudget(value: unknown, path: string): BudgetView {
  const r = record(value, path);
  return {
    max_model_calls: num(r.max_model_calls, `${path}.max_model_calls`),
    max_iterations: num(r.max_iterations, `${path}.max_iterations`),
    wall_clock_secs: num(r.wall_clock_secs, `${path}.wall_clock_secs`),
    max_debate_turns: num(r.max_debate_turns, `${path}.max_debate_turns`),
    max_tokens: num(r.max_tokens, `${path}.max_tokens`),
  };
}

function parseGoal(value: unknown, path: string): GoalView {
  const r = record(value, path);
  return {
    question: str(r.question, `${path}.question`),
    mode: str(r.mode, `${path}.mode`),
    revision: num(r.revision, `${path}.revision`),
    deliverables: strArray(r.deliverables, `${path}.deliverables`),
    budget: parseBudget(r.budget, `${path}.budget`),
  };
}

function parsePlan(value: unknown, path: string): PlanView | null {
  if (value === null || value === undefined) return null;
  const r = record(value, path);
  return {
    revision: num(r.revision, `${path}.revision`),
    roles: strArray(r.roles, `${path}.roles`),
    methods: strArray(r.methods, `${path}.methods`),
    rationale: str(r.rationale, `${path}.rationale`),
  };
}

function parseCounts(value: unknown, path: string): TaskCounts {
  const r = record(value, path);
  return {
    pending: num(r.pending, `${path}.pending`),
    blocked: num(r.blocked, `${path}.blocked`),
    running: num(r.running, `${path}.running`),
    waiting_for_human: num(r.waiting_for_human, `${path}.waiting_for_human`),
    completed: num(r.completed, `${path}.completed`),
    failed: num(r.failed, `${path}.failed`),
    cancelled: num(r.cancelled, `${path}.cancelled`),
    uncertain: num(r.uncertain, `${path}.uncertain`),
  };
}

function parseTaskFacts(value: unknown, path: string): TaskFacts | null {
  if (value === null || value === undefined) return null;
  const r = record(value, path);
  return {
    strategy: str(r.strategy, `${path}.strategy`),
    state: str(r.state, `${path}.state`),
    attempts: num(r.attempts, `${path}.attempts`),
    started_at: strOrNull(r.started_at, `${path}.started_at`),
    finished_at: strOrNull(r.finished_at, `${path}.finished_at`),
    error: strOrNull(r.error, `${path}.error`),
  };
}

function parseAssignments(value: unknown, path: string): Assignment[] | null {
  if (value === null || value === undefined) return null;
  return array(value, path).map((v, i) => {
    const r = record(v, `${path}[${i}]`);
    return {
      role: str(r.role, `${path}[${i}].role`),
      open: num(r.open, `${path}[${i}].open`),
      running: num(r.running, `${path}[${i}].running`),
      latest_strategy: strOrNull(r.latest_strategy, `${path}[${i}].latest_strategy`),
    };
  });
}

function parseAgent(value: unknown, path: string): AgentPanel {
  const r = record(value, path);
  return {
    role: str(r.role, `${path}.role`),
    state: agentState(r.state, `${path}.state`),
    counts: parseCounts(r.counts, `${path}.counts`),
    current: parseTaskFacts(r.current, `${path}.current`),
    last_finished: parseTaskFacts(r.last_finished, `${path}.last_finished`),
    assignments: parseAssignments(r.assignments, `${path}.assignments`),
  };
}

function parseLeader(value: unknown, path: string): Leader | null {
  if (value === null || value === undefined) return null;
  const r = record(value, path);
  return {
    item_id: str(r.item_id, `${path}.item_id`),
    title: str(r.title, `${path}.title`),
    rating: num(r.rating, `${path}.rating`),
    matches_played: num(r.matches_played, `${path}.matches_played`),
  };
}

function parseStats(value: unknown, path: string): Stats {
  const r = record(value, path);
  return {
    items: num(r.items, `${path}.items`),
    reviews: num(r.reviews, `${path}.reviews`),
    matches: num(r.matches, `${path}.matches`),
    clusters: num(r.clusters, `${path}.clusters`),
    sources: num(r.sources, `${path}.sources`),
    works: num(r.works, `${path}.works`),
    searches: num(r.searches, `${path}.searches`),
    pending_approvals: num(r.pending_approvals, `${path}.pending_approvals`),
    workers_busy: num(r.workers_busy, `${path}.workers_busy`),
    workers_total: num(r.workers_total, `${path}.workers_total`),
    leader: parseLeader(r.leader, `${path}.leader`),
  };
}

function parseUsage(value: unknown, path: string): BudgetUsage {
  const r = record(value, path);
  return {
    model_calls: num(r.model_calls, `${path}.model_calls`),
    tokens: num(r.tokens, `${path}.tokens`),
    tool_executions: num(r.tool_executions, `${path}.tool_executions`),
    reserved_calls: num(r.reserved_calls, `${path}.reserved_calls`),
    reserved_tokens: num(r.reserved_tokens, `${path}.reserved_tokens`),
    reserved_tool_executions: num(
      r.reserved_tool_executions,
      `${path}.reserved_tool_executions`,
    ),
    cost_usd: numOrNull(r.cost_usd, `${path}.cost_usd`),
    cost_partially_unknown: bool(
      r.cost_partially_unknown,
      `${path}.cost_partially_unknown`,
    ),
  };
}

function parseItem(value: unknown, path: string): ItemCard {
  const r = record(value, path);
  return {
    id: str(r.id, `${path}.id`),
    kind: str(r.kind, `${path}.kind`),
    title: str(r.title, `${path}.title`),
    disposition: str(r.disposition, `${path}.disposition`),
    assessment: str(r.assessment, `${path}.assessment`),
    rating: numOrNull(r.rating, `${path}.rating`),
    matches_played: num(r.matches_played, `${path}.matches_played`),
    stale_rating: bool(r.stale_rating, `${path}.stale_rating`),
    review_count: num(r.review_count, `${path}.review_count`),
  };
}

/** Validate a complete run snapshot (SSE payload and run detail). */
export function parseRunSnapshot(value: unknown): RunSnapshot {
  const r = record(value, "snapshot");
  return {
    run: parseRunMeta(r.run, "snapshot.run"),
    goal: parseGoal(r.goal, "snapshot.goal"),
    plan: parsePlan(r.plan, "snapshot.plan"),
    agents: array(r.agents, "snapshot.agents").map((a, i) =>
      parseAgent(a, `snapshot.agents[${i}]`),
    ),
    stats: parseStats(r.stats, "snapshot.stats"),
    usage: parseUsage(r.usage, "snapshot.usage"),
    items: array(r.items, "snapshot.items").map((it, i) =>
      parseItem(it, `snapshot.items[${i}]`),
    ),
  };
}

/** Validate the `GET /api/runs` payload. */
export function parseRunOverviews(value: unknown): RunOverview[] {
  const r = record(value, "runs response");
  return array(r.runs, "runs").map((v, i) => {
    const o = record(v, `runs[${i}]`);
    return {
      id: str(o.id, `runs[${i}].id`),
      state: runState(o.state, `runs[${i}].state`),
      stop_condition: strOrNull(o.stop_condition, `runs[${i}].stop_condition`),
      iterations: num(o.iterations, `runs[${i}].iterations`),
      question: str(o.question, `runs[${i}].question`),
      mode: str(o.mode, `runs[${i}].mode`),
      created_at: str(o.created_at, `runs[${i}].created_at`),
      updated_at: str(o.updated_at, `runs[${i}].updated_at`),
    };
  });
}

/** Validate a sources payload (per-run sources and the library). */
export function parseSources(value: unknown): SourceView[] {
  const r = record(value, "sources response");
  return array(r.sources, "sources").map((v, i) => {
    const s = record(v, `sources[${i}]`);
    const origin = record(s.origin, `sources[${i}].origin`);
    return {
      id: str(s.id, `sources[${i}].id`),
      run_id: str(s.run_id, `sources[${i}].run_id`),
      origin: {
        kind: str(origin.kind, `sources[${i}].origin.kind`),
        at: strOrNull(origin.at, `sources[${i}].origin.at`),
      },
      title: strOrNull(s.title, `sources[${i}].title`),
      authors: strOrNull(s.authors, `sources[${i}].authors`),
      date: strOrNull(s.date, `sources[${i}].date`),
      identifier: strOrNull(s.identifier, `sources[${i}].identifier`),
      retrieved_at: str(s.retrieved_at, `sources[${i}].retrieved_at`),
      content_hash: str(s.content_hash, `sources[${i}].content_hash`),
      access: str(s.access, `sources[${i}].access`),
      access_limitations: strOrNull(
        s.access_limitations,
        `sources[${i}].access_limitations`,
      ),
      text_artifact: strOrNull(s.text_artifact, `sources[${i}].text_artifact`),
    };
  });
}

/** Validate the report payload. */
export function parseReport(value: unknown): ReportView {
  const r = record(value, "report response");
  const manifest =
    r.manifest === null || r.manifest === undefined
      ? null
      : record(r.manifest, "report.manifest");
  return {
    run_id: str(r.run_id, "report.run_id"),
    markdown: str(r.markdown, "report.markdown"),
    manifest,
  };
}

function parseUploadView(value: unknown, path: string): UploadView {
  const r = record(value, path);
  return {
    id: str(r.id, `${path}.id`),
    file_name: str(r.file_name, `${path}.file_name`),
    size_bytes: num(r.size_bytes, `${path}.size_bytes`),
    content_hash: str(r.content_hash, `${path}.content_hash`),
    created_at: str(r.created_at, `${path}.created_at`),
  };
}

/** Validate the `POST /api/uploads` success envelope. */
export function parseUpload(value: unknown): UploadView {
  const r = record(value, "upload response");
  return parseUploadView(r.upload, "upload");
}

/** Validate the `GET /api/uploads` payload. */
export function parseUploads(value: unknown): UploadView[] {
  const r = record(value, "uploads response");
  return array(r.uploads, "uploads").map((v, i) =>
    parseUploadView(v, `uploads[${i}]`),
  );
}

/** Validate the `dry_run: true` response of `POST /api/runs`. */
export function parseRunPreview(value: unknown): RunPreview {
  const r = record(value, "preview response");
  const plan = record(r.plan, "preview.plan");
  const permissions = record(r.permissions, "preview.permissions");
  const budget = record(r.budget, "preview.budget");
  return {
    plan: {
      roles: strArray(plan.roles, "preview.plan.roles"),
      methods: strArray(plan.methods, "preview.plan.methods"),
      rationale: str(plan.rationale, "preview.plan.rationale"),
    },
    inputs: strArray(r.inputs, "preview.inputs"),
    permissions: {
      network: bool(permissions.network, "preview.permissions.network"),
      execute: bool(permissions.execute, "preview.permissions.execute"),
      allowed_tools: strArray(
        permissions.allowed_tools,
        "preview.permissions.allowed_tools",
      ),
    },
    budget: {
      max_model_calls: num(budget.max_model_calls, "preview.budget.max_model_calls"),
      max_seconds: num(budget.max_seconds, "preview.budget.max_seconds"),
      max_iterations: num(budget.max_iterations, "preview.budget.max_iterations"),
      max_debate_turns: num(
        budget.max_debate_turns,
        "preview.budget.max_debate_turns",
      ),
      max_acquisitions: num(
        budget.max_acquisitions,
        "preview.budget.max_acquisitions",
      ),
    },
  };
}

/** Validate the `POST /api/runs` success payload. */
export function parseStartedRun(value: unknown): string {
  const r = record(value, "start response");
  return str(r.run_id, "start response.run_id");
}

/**
 * Read the stable error body. Never throws: a non-conforming body (a proxy
 * error page, truncated JSON) becomes `kind: "unknown"` with whatever prose
 * is available, so error paths cannot themselves crash.
 */
export function parseApiError(value: unknown): ApiErrorBody {
  if (typeof value === "object" && value !== null && !Array.isArray(value)) {
    const r = value as Record<string, unknown>;
    if (typeof r.error === "string" && typeof r.kind === "string") {
      return { error: r.error, kind: r.kind };
    }
  }
  if (value === null || value === undefined) {
    return { error: "unrecognized error response", kind: "unknown" };
  }
  const text = typeof value === "string" ? value : JSON.stringify(value);
  return {
    error:
      text && text !== "undefined" && text !== "null"
        ? text.slice(0, 300)
        : "unrecognized error response",
    kind: "unknown",
  };
}
