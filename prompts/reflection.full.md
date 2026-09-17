You are performing a source-grounded review of a research item, including prior
work and any conflicting evidence.

Goal: {goal}

Criteria:
{preferences}

{instructions}

Item under review:
{item}

Sources available to you:
{articles_with_reasoning}

Source coverage actually available:
{source_coverage}

Evidence already linked to this item (execution results, supplied
observations, prior citations):
{evidence}

Execution available to this task: {execution}

If a step can only be settled by actually running something, and execution is
available, request it. Name only an approved program; inputs must be supplied
sources. The request is gated by the allowlist and by human approval, and its
result comes back as evidence for a later review to interpret.

Assess correctness, grounding, and completeness against the sources above.
Cite a locator for every sourced claim. Preserve contradictions and negative
results rather than selecting only supportive material. Distinguish what a
source reports from what you infer from it. Where the sources cannot settle a
point, record it as unresolved rather than filling the gap.

Respond with a single JSON object and no other text:

{
  "assessment_text": "<the review>",
  "proposed_assessment": "untested" | "plausible" | "contested" | "inconclusive",
  "evidence": [
    {"source_id": "<source id, or empty when citing an evidence record>",
     "evidence_id": "<existing evidence id, or null>",
     "locator": "<page/section/figure>",
     "reported": "<what the source or record actually reports there>",
     "claim": "<the specific claim this bears on>",
     "relation": "supports" | "contradicts" | "inconclusive" | "context",
     "justification": "<why, as your inference>", "limits": "<what it does not establish>"}
  ],
  "execution_requests": [
    {"program": "<approved program name>", "args": ["..."], "inputs": ["<supplied source path>"],
     "purpose": "<what the run would settle>", "target_claim": "<claim it bears on>"}
  ],
  "objections": [
    {"description": "...", "fatal": <bool>, "targets": "...",
     "suggested_check": "..."}
  ],
  "unknowns": ["..."],
  "next_actions": ["..."],
  "coverage_gaps": "<what material was unavailable or unsearched>"
}

Note: `proposed_assessment` here may not be `supported` or `refuted`. Those
require claim-relevant empirical evidence recorded separately; a review's
reading of the literature is not itself that evidence.
