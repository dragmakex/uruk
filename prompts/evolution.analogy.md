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
  "transferable_principle": "<the principle carried across>",
  "analogy_source": "<which inspiration, and what maps onto what>",
  "analogy_failure_modes": ["<where the analogy could break>", "..."],
  "discriminating_prediction": "<a prediction distinguishing this from its inspirations>",
  "borrowed_vs_new_evidence": "<what evidence supports the analogy source versus this mechanism>"
}
