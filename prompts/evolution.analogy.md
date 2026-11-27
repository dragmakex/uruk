You are an expert researcher generating one new hypothesis inspired by
analogous elements of the concepts supplied below.

Goal: {goal}

Criteria for a robust hypothesis:
{preferences}

Inspiration (draw analogy and inspiration, not direct replication):
{hypotheses}

Supporting material available:
{articles_with_reasoning}

Guidelines:
1. Summarise the relevant domain and what the supplied material actually
   evidences, with locators.
2. Identify the transferable principle behind the inspiring concepts — the
   structural reason they work, not their surface features.
3. CORE HYPOTHESIS: develop one detailed, original, specific hypothesis that
   applies that principle to the goal through a genuinely different mechanism.
   An aggregation or recombination of the inspirations is not acceptable;
   neither is a list of alternatives. Produce exactly one.
4. State where the analogy could break down — the disanalogy that would make
   the transfer fail.

Distinguish evidence that supports the inspiring concepts from evidence for
your new mechanism. Borrowed evidence does not transfer with the analogy.

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

Respond with a single JSON object and no other text:

{
  "title": "<short title>",
  "claim": "<the new hypothesis as a claim>",
  "mechanism": "<the genuinely new mechanism>",
  "assumptions": ["..."],
  "scope": "<where it applies>",
  "predictions": ["..."],
  "falsifiers": ["..."],
  "validation_plan": "<how to test it>",
  "grounding": [{"source_id": "<provided id>", "locator": "<where to read>", "supports": "<why to read this for the idea>"}],
  "transferable_principle": "<the principle carried across>",
  "analogy_source": "<which inspiration, and what maps onto what>",
  "analogy_failure_modes": ["<where the analogy could break>", "..."],
  "discriminating_prediction": "<a prediction distinguishing this from its inspirations>",
  "borrowed_vs_new_evidence": "<what evidence supports the analogy source versus this mechanism>"
}
