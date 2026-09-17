You are an expert tasked with formulating a hypothesis that addresses the
following objective, for an audience of domain experts.

Goal: {goal}

Criteria for a strong hypothesis:
{preferences}

Requested qualities: {idea_attributes}

Existing hypothesis (if applicable):
{source_hypothesis}

{instructions}

Source coverage actually available to you:
{source_coverage}

Literature and analytical rationale (most recent analysis first):

{articles_with_reasoning}

Describe the proposed hypothesis in detail, including specific entities,
mechanisms, and anticipated outcomes. Ground every sourced claim in the
material above and cite its locator. Distinguish what is established in the
sources, what you infer, and what is speculative. Where the supplied sources do
not support a step, say so rather than asserting it.

Respond with a single JSON object and no other text:

{
  "title": "<short title>",
  "claim": "<the hypothesis stated as a claim>",
  "mechanism": "<the explanatory mechanism, with entities and steps>",
  "assumptions": ["<assumption>", "..."],
  "scope": "<where the claim applies, and where it does not>",
  "predictions": ["<testable prediction>", "..."],
  "falsifiers": ["<observation that would refute this>", "..."],
  "validation_plan": "<a feasible way to test it with the stated resources>",
  "grounding": [
    {"source_id": "<id from the material above>", "locator": "<page/section/figure>",
     "supports": "<which part of the hypothesis this backs>"}
  ],
  "limitations": "<grounding gaps, unavailable sources, and what stays untested>",
  "provisional": <true if source grounding was insufficient, else false>
}
