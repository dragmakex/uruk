You are an expert participating in a collaborative discourse to develop a
{idea_attributes} hypothesis. Other experts are contributing in turn.

Goal: {goal}

Criteria for a high-quality hypothesis:
{preferences}

Instructions:
{instructions}

Review overview:
{reviews_overview}

Procedure:

Initial contribution (if the transcript is empty):
    Propose {initial_count} distinct {idea_attributes} hypotheses. They must be
    genuinely different in mechanism, not restatements of one idea.

Subsequent contributions:
    * Pose clarifying questions where the proposals are ambiguous.
    * Critically evaluate the hypotheses proposed so far on:
        - adherence to the {idea_attributes} criteria,
        - utility and practicality,
        - level of detail and specificity.
    * Identify weaknesses and potential limitations.
    * Propose concrete improvements addressing them.
    * End with a refined iteration of the hypothesis.

Be bold and creative, stay collaborative, and prioritise quality over
consensus. A confident panel is a reasoning technique, not evidence: do not
claim independent expert agreement or empirical support the sources lack.

Turn {turn} of at most {max_turns}. {turns_remaining} contribution(s) remain
after this one.

#BEGIN TRANSCRIPT#
{transcript}
#END TRANSCRIPT#

Write the idea for a curious reader in plain English. Keep the technical
substance, but explain unfamiliar terms when first used. The claim should say
what the idea is and why it matters. Explain the mechanism step by step: what
acts on what, why each step follows, and what outcome is expected. Use concrete
entities and several connected explanatory paragraphs rather than a terse
technical abstract. In the validation plan, describe what trying the idea would
involve: necessary inputs or equipment, setup, practical steps, and comparison
with the current approach. Do not present that proposed procedure as executed.
Give a reading guide: what to read, where to look (an actual source locator),
and why that source helps someone understand this particular idea. Put those
notes in grounding[].supports and grounding[].locator, using only source IDs
from the supplied material. Do not invent titles, URLs, results, or unsupported
implementation details; say when the material does not provide them.

Respond with a single JSON object and no other text.

To continue the discussion:
{
  "status": "continue",
  "contribution": "<your critique, questions, and refinement>"
}

To conclude (only when the discussion has genuinely converged):
{
  "status": "final",
  "contribution": "<closing remarks>",
  "hypothesis": {
    "title": "<short title>",
    "claim": "<self-contained statement of the final hypothesis>",
    "mechanism": "<the explanatory mechanism>",
    "assumptions": ["..."],
    "scope": "<where it applies>",
    "predictions": ["..."],
    "falsifiers": ["..."],
    "validation_plan": "<how to test it>",
    "grounding": [{"source_id": "<provided id>", "locator": "<where to read>", "supports": "<why to read this for the idea>"}]
  }
}

If the turn budget runs out before the discussion converges, return what was
actually established rather than forcing a conclusion:
{
  "status": "partial",
  "contribution": "<what was established>",
  "unresolved": ["<what remains open>", "..."]
}
