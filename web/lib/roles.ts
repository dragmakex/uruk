/** Stable role labels and responsibilities, not a mandatory execution order. */
export interface RoleMeta {
  id: string;
  label: string;
  description: string;
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
    description:
      "Plans research, allocates work and checks stopping conditions",
  },
  generation: {
    id: "generation",
    label: "Generation",
    description: "Literature-grounded ideas and simulated debate",
  },
  reflection: {
    id: "reflection",
    label: "Reflection",
    description: "Peer review and deep verification",
  },
  ranking: {
    id: "ranking",
    label: "Ranking",
    description: "Pairwise comparisons, scientific debate and Elo ranking",
  },
  proximity: {
    id: "proximity",
    label: "Proximity",
    description: "Groups related ideas and identifies overlap",
  },
  evolution: {
    id: "evolution",
    label: "Evolution",
    description: "Refines ideas and combines promising directions",
  },
  meta_review: {
    id: "meta_review",
    label: "Meta-review",
    description: "Synthesizes feedback and research overviews",
  },
  search: {
    id: "search",
    label: "Search",
    description: "Finds and acquires literature",
  },
  tool: {
    id: "tool",
    label: "Tool",
    description: "Runs permitted computational checks",
  },
};

export function roleLabel(role: string): string {
  return ROLES[role]?.label ?? role;
}

export function roleDescription(role: string): string {
  return ROLES[role]?.description ?? "";
}
