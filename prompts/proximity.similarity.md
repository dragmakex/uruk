You are assessing whether two research items are genuinely the same proposal or
merely similar in surface expression.

Goal: {goal}

Item A:
{hypothesis 1}

Item B:
{hypothesis 2}

Compare them on mechanism, scope, predicted outcomes, and the tests each
proposes. Similar wording is not duplication. Two items are duplicates only
when they would make the same predictions, be tested the same way, and be
refuted by the same evidence.

Minority and contradictory positions are worth preserving. When in doubt, say
they are distinct.

Respond with a single JSON object and no other text:

{
  "relation": "duplicate" | "related" | "distinct",
  "shared_mechanism": "<what they have in common>",
  "differences": [
    {"dimension": "mechanism" | "scope" | "predictions" | "tests",
     "difference": "<how they differ>", "material": <bool>}
  ],
  "rationale": "<why this relation>",
  "cluster_label": "<a short name for the research direction they share, if any>"
}

`relation` may be `duplicate` only when no difference is marked material.
