/** Shared test fixture: a complete snapshot as the Rust view model serializes it. */
export function snapshotFixture(): unknown {
  return {
    run: {
      id: "run_0412aa",
      state: "running",
      stop_condition: null,
      iterations: 1,
      created_at: "2026-10-04T14:00:00Z",
      updated_at: "2026-10-04T14:04:17Z",
      started_at: "2026-10-04T14:00:05Z",
      queue_position: null,
      paused_secs: 0,
      provider_wait: null,
      queue_provider_waiting: false,
    },
    goal: {
      question: "Which mechanisms could explain acquired resistance?",
      mode: "campaign",
      revision: 0,
      deliverables: ["research report"],
      budget: {
        max_model_calls: 200,
        max_iterations: 4,
        max_debate_turns: 5,
      },
    },
    plan: {
      revision: 0,
      roles: ["generation", "reflection", "ranking"],
      methods: ["grounded generation"],
      rationale: "campaign plan",
    },
    agents: [
      {
        role: "supervisor",
        state: "thinking",
        counts: {
          pending: 1,
          blocked: 0,
          running: 0,
          completed: 2,
          failed: 0,
          cancelled: 0,
          uncertain: 0,
        },
        current: null,
        last_finished: null,
        assignments: [
          { role: "generation", open: 3, running: 1, latest_strategy: "literature" },
        ],
      },
      {
        role: "generation",
        state: "thinking",
        counts: {
          pending: 2,
          blocked: 0,
          running: 1,
          completed: 4,
          failed: 0,
          cancelled: 0,
          uncertain: 0,
        },
        current: {
          id: "task-generation-literature",
          reasoning_preview: "Inspect the strongest sources before comparing the hypotheses.",
          reasoning_streaming: true,
          strategy: "literature",
          state: "running",
          attempts: 1,
          started_at: "2026-10-04T14:04:00Z",
          finished_at: null,
          error: null,
        },
        last_finished: null,
        assignments: null,
      },
    ],
    stats: {
      items: 13,
      reviews: 4,
      matches: 34,
      decisions: 5,
      clusters: 4,
      sources: 41,
      works: 0,
      searches: 0,
      workers_busy: 3,
      workers_total: 4,
      leader: {
        item_id: "item_h7",
        title: "XBP1 splicing escape",
        rating: 1412.2,
        matches_played: 9,
      },
    },
    usage: {
      model_calls: 12,
      tokens: 48000,
      tool_executions: 0,
      reserved_calls: 2,
      cost_usd: null,
      cost_partially_unknown: false,
    },
    items: [
      {
        id: "item_h7",
        kind: "hypothesis",
        title: "XBP1 splicing escape",
        disposition: "active",
        assessment: "plausible",
        rating: 1412.2,
        matches_played: 9,
        stale_rating: false,
        review_count: 3,
      },
    ],
  };
}

/**
 * A tournament feed as the API reports it: two matches, one decided and
 * one declined for lack of basis, so both rating movements a log can show
 * (a real shift and a deliberate zero) are covered by one fixture.
 */
export function tournamentFixture(): unknown {
  return {
    run_id: "run_0412aa",
    matches: [
      {
        id: "match_01",
        method: "pairwise",
        judge_model: "deepseek-v4.1-flash",
        outcome: "win_a",
        rationale:
          "The first candidate names the escape mechanism and the readout that would falsify it; the second only restates the premise.",
        first: {
          item_id: "item_h7",
          title: "XBP1 splicing escape",
          rating_before: 1200.0,
          rating_after: 1216.0,
          delta: 16.0,
        },
        second: {
          item_id: "item_k2",
          title: "IRE1 armouring",
          rating_before: 1200.0,
          rating_after: 1184.0,
          delta: -16.0,
        },
        order_randomized: true,
        created_at: "2026-10-04T14:02:11Z",
        produced_by: "task_m01",
      },
      {
        id: "match_02",
        method: "debate",
        judge_model: "deepseek-v4.1-flash",
        outcome: "insufficient_basis",
        rationale:
          "Both sides cite the same two papers and neither defends a distinct prediction, so there is nothing to separate them.",
        first: {
          item_id: "item_k2",
          title: "IRE1 armouring",
          rating_before: 1184.0,
          rating_after: 1184.0,
          delta: 0,
        },
        second: {
          item_id: "item_m9",
          title: "PERK titration",
          rating_before: 1200.0,
          rating_after: 1200.0,
          delta: 0,
        },
        order_randomized: false,
        created_at: "2026-10-04T14:06:40Z",
        produced_by: null,
      },
    ],
  };
}
