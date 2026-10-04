/**
 * Static role metadata for the topology and the agent legend. The
 * descriptions state what each engine role actually does (SPEC §5); the
 * `feeds` field is the pipeline context shown when connection wires are
 * hidden on narrow viewports.
 */

export interface RoleMeta {
  /** Role name as the API reports it. */
  id: string;
  /** Display label as the design writes it. */
  label: string;
  /** What the role does, factually. */
  description: string;
  /** Downstream role in the work pipeline, for wire-less viewports. */
  feeds: string | null;
}

export const ROLE_ORDER: readonly string[] = [
  "supervisor",
  "generation",
  "reflection",
  "ranking",
  "proximity",
  "evolution",
  "meta_review",
];

export const ROLES: Record<string, RoleMeta> = {
  supervisor: {
    id: "supervisor",
    label: "Supervisor",
    description: "splits the goal, assigns work",
    feeds: "generation",
  },
  generation: {
    id: "generation",
    label: "Generation",
    description: "reads, drafts hypotheses",
    feeds: "reflection",
  },
  reflection: {
    id: "reflection",
    label: "Reflection",
    description: "reviews novelty and soundness",
    feeds: "ranking",
  },
  ranking: {
    id: "ranking",
    label: "Ranking",
    description: "runs the matches, keeps Elo",
    feeds: "meta_review",
  },
  proximity: {
    id: "proximity",
    label: "Proximity",
    description: "clusters similar ideas",
    feeds: "evolution",
  },
  evolution: {
    id: "evolution",
    label: "Evolution",
    description: "rewrites the strongest ideas",
    feeds: "meta_review",
  },
  meta_review: {
    id: "meta_review",
    label: "Meta-review",
    description: "summarises each round",
    feeds: null,
  },
  search: {
    id: "search",
    label: "Search",
    description: "finds and acquires literature",
    feeds: "generation",
  },
  tool: {
    id: "tool",
    label: "Tool",
    description: "runs approved executions",
    feeds: "reflection",
  },
};

export function roleLabel(role: string): string {
  return ROLES[role]?.label ?? role;
}

export function roleDescription(role: string): string {
  return ROLES[role]?.description ?? "";
}

/** Wires of the topology figure: solid data-flow edges between roles. */
export const WIRES: ReadonlyArray<readonly [string, string]> = [
  ["supervisor", "generation"],
  ["supervisor", "proximity"],
  ["generation", "reflection"],
  ["reflection", "ranking"],
  ["proximity", "evolution"],
  ["evolution", "meta_review"],
  ["ranking", "meta_review"],
];
