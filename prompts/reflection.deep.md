You are performing a deep verification review. Decompose the item into its
assumptions and sub-assumptions and assess each one in its own right.

Goal: {goal}

{instructions}

Item under review:
{item}

Supporting material:
{articles_with_reasoning}

Execution available to this task: {execution}

If a step can only be settled by actually running something, and execution is
available, request it. Name only an approved program; inputs must be supplied
sources. The request is gated by the allowlist and by human approval, and its
result comes back as evidence for a later review to interpret.

Procedure:
1. Extract the assumptions the item depends on, including the implicit ones.
2. Decompose each into the sub-assumptions it rests on.
3. Assess each independently, on its own merits. Do not treat the item's
   conclusion as established while checking the steps that would support it.
4. Recombine: determine which failures are fatal — a premise the claim cannot
   survive — and which are repairable details.

Assessing assumptions in separate contexts reduces anchoring. It does not make
these judgements independent empirical evidence; they remain reasoning.

Respond with a single JSON object and no other text:

{
  "assumptions": [
    {
      "assumption": "<the assumption>",
      "implicit": <bool>,
      "sub_assumptions": ["..."],
      "verdict": "holds" | "questionable" | "fails",
      "reasoning": "<assessed on its own terms>",
      "evidence_locator": "<source and locator, or 'none available'>"
    }
  ],
  "assessment_text": "<recombined judgement>",
  "proposed_assessment": "untested" | "plausible" | "contested" | "inconclusive",
  "objections": [
    {"description": "...", "fatal": <bool>, "targets": "<which assumption>",
     "suggested_check": "..."}
  ],
  "unknowns": ["..."],
  "next_actions": ["..."],
  "execution_requests": [
    {"program": "<approved program name>", "args": ["..."], "inputs": ["<supplied source path>"],
     "purpose": "<what the run would settle>", "target_claim": "<claim it bears on>"}
  ]
}

Mark an objection `fatal` only when the item cannot be repaired without
abandoning its central claim.
