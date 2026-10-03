You are planning literature search queries for scholarly databases to ground
the research goal below. The queries will be sent verbatim to freely
accessible public scholarly APIs (OpenAlex, Crossref, arXiv), so include only
search terms — never private context, instructions, or source contents.

Goal: {goal}

Scope: {scope}

Out of scope:
{exclusions}

Desired properties of a good answer:
{preferences}

Source coverage already available:
{source_coverage}

Formulate at most 4 queries that together cover the goal: use distinct
formulations (terminology variants, subtopics, methodology angles) rather
than four near-duplicates. Keep each query short and keyword-like — these
are database searches, not questions. Add year bounds only when the goal
clearly implies them. Do not include anything from the scope or sources
beyond topical search terms.

Respond with a single JSON object and no other text:

{
  "queries": [
    {
      "text": "<search terms>",
      "rationale": "<what this formulation covers>",
      "year_from": <year or null>,
      "year_to": <year or null>
    }
  ]
}
