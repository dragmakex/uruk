You are producing the requested deliverable from the recorded research
material, for a curious reader who wants to understand and explore the ideas.

Goal: {goal}

Requested deliverable: {deliverable}

Criteria:
{preferences}

{instructions}

Sources available:
{articles_with_reasoning}

Source coverage actually available:
{source_coverage}

Recorded items, evidence, and reviews:
{records}

Write the deliverable using only the material above. Every substantive sourced
claim carries a citation with its locator. Keep three things visibly separate:
what the sources report, what you infer from them, and what remains an untested
proposal.

Preserve disagreements between sources, negative results, and gaps in coverage.
Do not invent citations, experiments, sample sizes, or results, and do not
imply you read full text where only an abstract was available.

Write in plain English without losing technical substance. Explain the ideas
step by step: what each is, why it could help, how it would actually work, and
what trying it would involve. Explain unfamiliar terms as you introduce them.
Provide what to read for each idea, with titles, available source links,
locators, and a reason to read each source, only when provided in the material.
Do not substitute a field list of claims, falsifiers, derivation/change logs,
or internal IDs for the explanation. Keep important uncertainty in ordinary
language. When rankings are available, discuss the leading five active ideas
in the recorded Elo order; do not invent a ranking or include five just to fill
a quota. Keep audit detail in the structured fields below.

Respond with a single JSON object and no other text:

{
  "title": "<title of the deliverable>",
  "body": "<the deliverable in Markdown, with inline citations as [source_id, locator]>",
  "claims": [
    {"claim": "<a substantive claim made in the body>",
     "basis": "source_reported" | "uruk_inference" | "untested_proposal",
     "citations": [{"source_id": "<id>", "locator": "<locator>"}]}
  ],
  "disagreements": ["<where sources conflict, and how>", "..."],
  "limitations": "<coverage, access, methodological, and validation gaps>",
  "next_actions": ["<what would require human judgement or further work>", "..."]
}
