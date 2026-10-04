/**
 * TypeScript mirrors of the Rust view models (`src/web/view.rs`) and record
 * shapes the API serves. These types are only trusted after passing through
 * the runtime validators in `lib/parse.ts`; raw `fetch` JSON is never cast.
 */

export type RunState =
  | "running"
  | "waiting-for-human"
  | "blocked"
  | "completed"
  | "budget-exhausted"
  | "cancelled"
  | "failed";

export const RUN_STATES: readonly RunState[] = [
  "running",
  "waiting-for-human",
  "blocked",
  "completed",
  "budget-exhausted",
  "cancelled",
  "failed",
];

export const TERMINAL_STATES: readonly RunState[] = [
  "completed",
  "budget-exhausted",
  "cancelled",
  "failed",
];

export type AgentState = "thinking" | "waiting" | "idle";

export interface RunMeta {
  id: string;
  state: RunState;
  stop_condition: string | null;
  iterations: number;
  created_at: string;
  updated_at: string;
}

export interface BudgetView {
  max_model_calls: number;
  max_iterations: number;
  wall_clock_secs: number;
  max_debate_turns: number;
  max_tokens: number;
}

export interface GoalView {
  question: string;
  mode: string;
  revision: number;
  deliverables: string[];
  budget: BudgetView;
}

export interface PlanView {
  revision: number;
  roles: string[];
  methods: string[];
  rationale: string;
}

export interface TaskCounts {
  pending: number;
  blocked: number;
  running: number;
  waiting_for_human: number;
  completed: number;
  failed: number;
  cancelled: number;
  uncertain: number;
}

export interface TaskFacts {
  strategy: string;
  state: string;
  attempts: number;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

export interface Assignment {
  role: string;
  open: number;
  running: number;
  latest_strategy: string | null;
}

export interface AgentPanel {
  role: string;
  state: AgentState;
  counts: TaskCounts;
  current: TaskFacts | null;
  last_finished: TaskFacts | null;
  assignments: Assignment[] | null;
}

export interface Leader {
  item_id: string;
  title: string;
  rating: number;
  matches_played: number;
}

export interface Stats {
  items: number;
  reviews: number;
  matches: number;
  clusters: number;
  sources: number;
  works: number;
  searches: number;
  pending_approvals: number;
  workers_busy: number;
  workers_total: number;
  leader: Leader | null;
}

export interface BudgetUsage {
  model_calls: number;
  tokens: number;
  tool_executions: number;
  reserved_calls: number;
  reserved_tokens: number;
  reserved_tool_executions: number;
  cost_usd: number | null;
  cost_partially_unknown: boolean;
}

export interface ItemCard {
  id: string;
  kind: string;
  title: string;
  disposition: string;
  assessment: string;
  rating: number | null;
  matches_played: number;
  stale_rating: boolean;
  review_count: number;
}

export interface RunSnapshot {
  run: RunMeta;
  goal: GoalView;
  plan: PlanView | null;
  agents: AgentPanel[];
  stats: Stats;
  usage: BudgetUsage;
  items: ItemCard[];
}

export interface RunOverview {
  id: string;
  state: RunState;
  stop_condition: string | null;
  iterations: number;
  question: string;
  mode: string;
  created_at: string;
  updated_at: string;
}

export interface SourceOrigin {
  kind: string;
  at: string | null;
}

export interface SourceView {
  id: string;
  run_id: string;
  origin: SourceOrigin;
  title: string | null;
  authors: string | null;
  date: string | null;
  identifier: string | null;
  retrieved_at: string;
  content_hash: string;
  access: string;
  access_limitations: string | null;
  text_artifact: string | null;
}

export interface ReportView {
  run_id: string;
  markdown: string;
  manifest: Record<string, unknown> | null;
}

/** The stable error body every API error returns. */
export interface ApiErrorBody {
  error: string;
  kind: string;
}

export interface StartRunInput {
  goal: string;
  mode: "task" | "campaign";
  ranking: "simple" | "tournament";
}
