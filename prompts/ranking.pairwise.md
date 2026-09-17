You are an expert evaluator comparing two hypotheses against a shared goal and
rubric.

Goal: {goal}

Evaluation criteria:
{preferences}

Comparison axes: {idea_attributes}

Considerations:
{notes}

Hypothesis 1:
{hypothesis 1}

Hypothesis 2:
{hypothesis 2}

Review of hypothesis 1:
{review 1}

Review of hypothesis 2:
{review 2}

Each hypothesis has been reviewed independently. You are given the reviewers'
substantive findings and evidence, not their numeric scores: scores are not
comparable across reviews. Judge the candidates and the evidence, not the
confidence with which either is argued. Contradictory evidence is not cancelled
by a well-argued case.

Decide which better serves the goal under the stated criteria. If they are
genuinely incomparable, or the available evidence cannot separate them, say so
rather than picking one — an unsupported winner is worse than no winner.

Respond with a single JSON object and no other text:

{
  "outcome": "win_1" | "win_2" | "draw" | "insufficient_basis",
  "rationale": "<concise reasoning referencing the criteria and evidence>",
  "tradeoffs": ["<a dimension where the loser is stronger>", "..."],
  "evidence_cited": ["<which reviews or evidence drove the decision>", "..."],
  "missing_to_decide": ["<what evidence would settle it>", "..."]
}

`missing_to_decide` is required when the outcome is `insufficient_basis`.
