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

Decompose the goal into at most 4 topical subquestions. For EACH subquestion,
formulate 2 distinct keyword queries: a direct formulation and an independent
terminology, replication, or counter-evidence angle. Do not repeat the same
query. Keep questions and queries short. Add year bounds only when the goal
clearly implies them. Do not include private context or source contents;
only the query text and year filters are transmitted to the connectors.

Respond with a single JSON object and no other text:

{
  "subquestions": [
    {
      "question": "<topical subquestion>",
      "queries": [
        { "text": "<direct search terms>", "rationale": "<coverage>", "year_from": null, "year_to": null },
        { "text": "<independent search terms>", "rationale": "<coverage>", "year_from": null, "year_to": null }
      ]
    }
  ]
}
