You are walking through the mechanism or protocol step by step to find where it
would fail in practice.

Goal: {goal}

{instructions}

Item under review:
{item}

Execution available to this task: {execution}

If a step can only be settled by actually running something, and execution is
available, request it. Name only an approved program; inputs must be supplied
sources. The request is gated by the allowlist and by human approval, and its
result comes back as evidence for a later review to interpret.

Trace the mechanism from its starting conditions to its predicted outcome. At
each step state what must hold, what could go wrong, and what would be observed
if it did.

This is a reasoned walkthrough, not an executed simulation. Nothing here was
run and nothing here measured anything. Report it as reasoning; if a step can
only be settled by actually running something, say so and name the check.

Respond with a single JSON object and no other text:

{
  "steps": [
    {"step": "<what happens>", "requires": "<what must hold>",
     "failure_mode": "<how it could fail>",
     "observable": "<what would be seen if it failed>"}
  ],
  "assessment_text": "<overall walkthrough judgement>",
  "proposed_assessment": "untested" | "plausible" | "inconclusive",
  "objections": [
    {"description": "...", "fatal": <bool>, "targets": "...",
     "suggested_check": "..."}
  ],
  "unknowns": ["..."],
  "next_actions": ["<the execution or measurement that would settle a step>"],
  "requires_execution": ["<step that cannot be resolved by reasoning alone>"],
  "execution_requests": [
    {"program": "<approved program name>", "args": ["..."], "inputs": ["<supplied source path>"],
     "purpose": "<what the run would settle>", "target_claim": "<claim it bears on>"}
  ]
}
