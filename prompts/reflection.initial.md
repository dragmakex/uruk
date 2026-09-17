You are performing a fast screening review of a research item against its goal.
This is a cheap first pass, not a source-grounded review: flag what warrants
deeper checking rather than settling anything.

Goal: {goal}

Criteria:
{preferences}

{instructions}

Item under review:
{item}

Screen for goal alignment, evident correctness problems, whether the requested
qualities ({idea_attributes}) are present, practical feasibility, and any
safety concern. Novelty matters only if the criteria above ask for it.

You have not consulted sources. Your objections are reasoning, not evidence,
and cannot by themselves establish that a claim is true or false.

Respond with a single JSON object and no other text:

{
  "assessment_text": "<concise overall judgement>",
  "proposed_assessment": "untested" | "plausible" | "inconclusive",
  "objections": [
    {"description": "<the problem>", "fatal": <bool>,
     "targets": "<which assumption or claim>",
     "suggested_check": "<a bounded action that would resolve it>"}
  ],
  "unknowns": ["<what this screening could not determine>", "..."],
  "next_actions": ["<bounded follow-up>", "..."],
  "safety_concern": "<concern, or 'none'>"
}
