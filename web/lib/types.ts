/**
 * TypeScript mirrors of the Rust view models (`src/web/view.rs`) and record
 * shapes the API serves. These types are only trusted after passing through
 * the runtime validators in `lib/parse.ts`; raw `fetch` JSON is never cast.
 */

export type RunState =
  | "queued"
  | "running"
  | "completed"
  | "budget-exhausted"
  | "cancelled"
  | "failed";

export const RUN_STATES: readonly RunState[] = [
  "queued",
  "running",
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

/** A run's open wait for a transiently unavailable model provider. */
export interface ProviderWait {
  /** When the upstream was last seen unavailable (RFC 3339). */
  since: string;
  /** The transient failure last observed, as the adapter reported it. */
  note: string | null;
}

export interface RunMeta {
  id: string;
  state: RunState;
  stop_condition: string | null;
  iterations: number;
  created_at: string;
  updated_at: string;
  /** When the run first executed; null while queued. */
  started_at: string | null;
  /** Place on the run queue while `queued` (1 = next to start); null otherwise. */
  queue_position: number | null;
  /** Seconds of settled provider waits. */
  paused_secs: number;
  /** The outage being waited out; the run is still `running`, not failed. */
  provider_wait: ProviderWait | null;
  /**
   * While this run holds a queue place (`queue_position` is set):
   * whether the run being executed is waiting out a provider outage,
   * so the queued researcher knows why nothing moves. One bit only:
   * never which run, whose, since when, or what the provider said.
   * False after recovery, with nothing active, and off the queue.
   */
  queue_provider_waiting: boolean;
}

export interface BudgetView {
  max_model_calls: number;
  max_iterations: number;
  max_debate_turns: number;
}

export interface GoalView {
  question: string;
  mode: string;
  revision: number;
  deliverables: string[];
  rubric: {
    preferences: string[];
    attributes: string[];
    constraints: string[];
    acceptance: string[];
  };
  /** Permissions recorded for this run, not tool availability or health. */
  permissions: {
    network: boolean;
    execute: boolean;
    allowed_tools: string[];
  };
  budget: BudgetView;
}

export interface PlanView {
  revision: number;
  roles: string[];
  methods: string[];
  rationale: string;
  stopping_conditions: string[];
}

export interface TaskCounts {
  pending: number;
  blocked: number;
  running: number;
  completed: number;
  failed: number;
  cancelled: number;
  uncertain: number;
}

export interface TaskFacts {
  id: string;
  reasoning_preview: string | null;
  reasoning_streaming: boolean;
  strategy: string;
  state: string;
  attempts: number;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

export interface ReasoningTrace {
  started_at: string;
  finished_at: string | null;
  model: string | null;
  content: string;
  streaming: boolean;
}

export interface TaskReasoning {
  task: {
    id: string;
    role: string;
    strategy: string;
    state: string;
  };
  reasoning: ReasoningTrace[];
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
  /** Decisions on the record; the ledger re-reads when this changes. */
  decisions: number;
  clusters: number;
  sources: number;
  works: number;
  searches: number;
  workers_busy: number;
  workers_total: number;
  leader: Leader | null;
}

export interface BudgetUsage {
  model_calls: number;
  tokens: number;
  tool_executions: number;
  reserved_calls: number;
  cost_usd: number | null;
  cost_partially_unknown: boolean;
}

export interface AssessmentBasis {
  rationale: string;
  review_id: string | null;
  decision_id: string | null;
  evidence: Array<{
    id: string;
    label: string;
    kind: string;
    relation: string;
    claim: string;
    limits: string | null;
  }>;
}

export interface ItemCard {
  id: string;
  kind: string;
  title: string;
  disposition: string;
  assessment: string;
  assessment_basis: AssessmentBasis | null;
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
  ranking_baseline: { iteration: number; ratings: Record<string, number> } | null;
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
  /** Place on the run queue while `queued` (1 = next to start); null otherwise. */
  queue_position: number | null;
  /** The open provider wait of a running run (RFC 3339), or null. */
  paused_since: string | null;
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

/** One run's source with how many passages its text contributes. */
export interface SourceDetail {
  run_id: string;
  source: SourceView;
  passage_count: number;
}

/** One stored passage in artifact order (browsing a source's text). */
export interface PassageView {
  seq: number;
  /** Byte offsets into the canonical UTF-8 artifact text. */
  byte_start: number;
  byte_end: number;
  text: string;
}

/** One browse page of a source's passages. */
export interface PassagePage {
  total: number;
  offset: number;
  limit: number;
  passages: PassageView[];
}

/** One BM25-ranked hit of a within-source passage search. */
export interface SourcePassageHit {
  source_id: string;
  seq: number;
  byte_start: number;
  byte_end: number;
  text: string;
  /** FTS5 snippet, plain text with `…` ellipsis. */
  snippet: string;
  /** Higher is better. */
  score: number;
}

/** A ranked hit of the owner-wide search, naming its run and source. */
export interface LibraryPassageHit extends SourcePassageHit {
  run_id: string;
  source_title: string | null;
}

/** Metadata filters for the library listing; absent means no filter. */
export interface LibraryFilter {
  access?: string;
  origin?: string;
  q?: string;
}

export type CitationOriginView =
  | { kind: "deliverable_claim"; basis: string }
  | { kind: "review_observation"; review_id: string };

/**
 * The three honest resolution shapes (`src/report/citations.rs`): exact
 * verified bytes, a coarse locator on a recorded source, or an explicit
 * failure with its reason. There is no fourth, approximate shape.
 */
export type CitationResolutionView =
  | { kind: "span"; start: number; end: number; text: string; truncated: boolean }
  | { kind: "source_locator" }
  | { kind: "unresolved"; reason: string };

export interface CitationView {
  origin: CitationOriginView;
  claim: string;
  source_id: string;
  source_title: string | null;
  locator: string | null;
  resolution: CitationResolutionView;
}

export interface ReportView {
  run_id: string;
  markdown: string;
  manifest: Record<string, unknown> | null;
}

/**
 * One decision on the record: who took it, what kind it was, and why in
 * their own words. The structured payload stays on the server — it is
 * shaped per kind, and rendering it as prose would invent a reading of it.
 */
export interface DecisionView {
  id: string;
  /** `scheduling`, `disposition`, `meta_feedback` or `stop`. Kept as a
   *  string: an unknown kind from a newer engine still renders as itself
   *  rather than disappearing. */
  kind: string;
  /** `researcher`, `supervisor`, `system`, or an agent's own name. */
  actor: string;
  reason: string;
  referenced: string[];
  created_at: string;
}

/** Bounds for one explicitly requested record page. */
export interface PageRequest {
  offset: number;
  limit: number;
  through?: number;
}

/** Server-applied bounds; histories pin an append-only insertion watermark. */
export interface PageInfo {
  total: number;
  offset: number;
  limit: number;
  through: number | null;
}

export interface CandidatePage extends PageInfo {
  run_id: string;
  items: ItemCard[];
}

/** One page of decisions on the run's record, oldest first. */
export interface DecisionLedger extends PageInfo {
  run_id: string;
  decisions: DecisionView[];
}

/**
 * What one match meant from a particular candidate's side. Stated by the
 * service, so the UI never has to work out which side was `item_a`.
 * `unresolved` is a verdict of insufficient basis: real, but no rating
 * moved and no winner was named.
 */
export type HistoryResult = "won" | "lost" | "drew" | "unresolved";

/** One match as it reads in a single candidate's history. */
export interface HistoryEntry {
  id: string;
  opponent_id: string;
  /** The opponent's title, or its ID when the record no longer has one. */
  opponent_title: string;
  result: HistoryResult;
  method: MatchMethod;
  judge_model: string;
  /** The judge's stated reason, in its own words. */
  rationale: string;
  rating_before: number;
  rating_after: number;
  /** `rating_after - rating_before`, signed. Zero for `unresolved`. */
  delta: number;
  created_at: string;
  produced_by: string | null;
}

/**
 * Every match the record has for one candidate, newest first. An empty list
 * means the candidate has not fought yet; it does not mean the read failed.
 */
export interface CandidateHistory {
  run_id: string;
  item_id: string;
  title: string;
  matches: HistoryEntry[];
}

/**
 * One side of a recorded comparison, with the rating it entered and left
 * with. `delta` is what the match did to that candidate, and it is zero
 * when the verdict updated nothing (an insufficient basis).
 */
export interface TournamentSide {
  item_id: string;
  /** The candidate's title, or its ID when the record no longer has one. */
  title: string;
  rating_before: number;
  rating_after: number;
  delta: number;
}

/**
 * Which side won, in presentation order. `first` is the candidate the judge
 * saw first (`item_a`), so a verdict maps onto a stable ID; `win_a` means
 * the first side won, not that a particular candidate won.
 */
export type MatchOutcome = "win_a" | "win_b" | "draw" | "insufficient_basis";

export const MATCH_OUTCOMES: readonly MatchOutcome[] = [
  "win_a",
  "win_b",
  "draw",
  "insufficient_basis",
];

/** How a comparison was conducted: one turn, or a multi-turn debate. */
export type MatchMethod = "pairwise" | "debate";

export const MATCH_METHODS: readonly MatchMethod[] = ["pairwise", "debate"];

/** One recorded match, as the tournament feed reports it. */
export interface TournamentMatch {
  id: string;
  method: MatchMethod;
  /** Model and configuration that judged. */
  judge_model: string;
  outcome: MatchOutcome;
  /** The judge's stated reason, in its own words. */
  rationale: string;
  first: TournamentSide;
  second: TournamentSide;
  /** True when the pair was shuffled before presentation. */
  order_randomized: boolean;
  created_at: string;
  /** The task that played the match, if the record names one. */
  produced_by: string | null;
}

export interface TournamentFeed extends PageInfo {
  run_id: string;
  /** Only the requested slice, newest first with deterministic ID tie breaks. */
  matches: TournamentMatch[];
}

/** The stable error body every API error returns. */
export interface ApiErrorBody {
  error: string;
  kind: string;
}

/**
 * Everything `POST /api/runs` accepts (`src/web/start.rs`). Only `goal`,
 * `mode`, and `ranking` are always sent; the rest ride along when the
 * form sets them. There is deliberately no field here for a filesystem
 * path or an execution grant — files travel as owner-bound uploads and
 * the web surface never grants execution.
 */
export interface StartRunInput {
  goal: string;
  mode: "task" | "campaign";
  ranking: "simple" | "tournament";
  profile?: string;
  deliverables?: string[];
  preferences?: string[];
  attributes?: string[];
  constraints?: string[];
  max_model_calls?: number;
  max_iterations?: number;
  max_debate_turns?: number;
  max_acquisitions?: number;
  input_urls?: string[];
  upload_ids?: string[];
  allow_network?: boolean;
  search?: boolean;
  search_connectors?: string[];
  dry_run?: boolean;
}

/** An owner-bound upload as the API reports it; storage stays server-side. */
export interface UploadView {
  id: string;
  file_name: string;
  size_bytes: number;
  content_hash: string;
  created_at: string;
}

/** The `dry_run: true` response: the plan a start would dispatch. */
export interface RunPreview {
  plan: {
    roles: string[];
    methods: string[];
    rationale: string;
  };
  inputs: string[];
  permissions: {
    network: boolean;
    execute: boolean;
    allowed_tools: string[];
  };
  budget: {
    max_model_calls: number;
    max_iterations: number;
    max_debate_turns: number;
    max_acquisitions: number;
  };
}
