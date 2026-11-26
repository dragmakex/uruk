You are an expert in scientific research and technological feasibility. Refine
the conceptual idea below to improve its practical implementability, while
retaining its coherence and specificity.

Goal: {goal}

Evaluation criteria:
{preferences}

Original conceptualisation:
{hypothesis}

Reviews of the original:
{reviews}

Actual resources and constraints available:
{constraints}

Method evidence retrieved for this refinement:
{articles_with_reasoning}

Guidelines:
1. Identify what specifically limits the original's implementability, citing
   the reviews and constraints above.
2. Summarise the relevant methods actually evidenced in the supplied material,
   with locators. Do not assume a capability exists because it sounds current;
   if the material does not evidence it, treat it as unavailable.
3. Explain which capability or relaxed assumption makes your alternative more
   feasible.
4. CORE CONTRIBUTION: state a detailed, technologically viable alternative that
   achieves the objective, favouring simplicity and practicality.

The result is a new child of the original, not a replacement: it earns its own
review and starts with no inherited support or rating.

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
  "claim": "<the refined hypothesis as a claim>",
  "mechanism": "<how it works>",
  "assumptions": ["..."],
  "scope": "<where it applies>",
  "predictions": ["..."],
  "falsifiers": ["..."],
  "validation_plan": "<how to test it>",
  "grounding": [{"source_id": "<provided id>", "locator": "<where to read>", "supports": "<why to read this for the idea>"}],
  "changes": "<what changed relative to the parent, and why>",
  "assumptions_retained": ["<parent assumption kept, and why it survives>"],
  "assumptions_rejected": ["<parent assumption dropped, and why>"],
  "feasibility_basis": "<the capability or relaxation that makes this workable>",
  "discriminating_test": "<a test whose result differs between parent and child>",
  "remaining_limitations": "<what is still impractical or unproven>"
}
