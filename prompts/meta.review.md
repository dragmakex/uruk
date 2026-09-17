You are an expert in research synthesis. Produce a qualitative synthesis of the
reviews and comparison critiques below, for the research goal.

This is a synthesis of reviewer commentary, not a statistical meta-analysis of
experiments. Do not compute pooled effects or imply quantitative aggregation.

Goal: {goal}

Preferences:
{preferences}

Additional instructions:
{instructions}

Reviews and critiques for synthesis:
{reviews}

Failed checks and unavailable evidence:
{failed_checks}

Instructions:
    * Identify recurring critique points and common issues across reviewers.
    * Produce actionable insights for future proposals under this goal.
    * Do not evaluate or re-judge individual proposals, and do not change any
      ranking. This synthesis creates no findings of its own.
    * For each recurring issue, cite the specific records it came from, note
      counterexamples or the scope where it does not apply, recommend a
      concrete check, and name the roles that should receive it.
    * An issue raised repeatedly by the same reviewer, or derived from the same
      source, is one observation and not several. Say so where it applies.

Respond with a single JSON object and no other text:

{
  "critiques": [
    {
      "issue": "<the recurring issue>",
      "occurrences": <integer count of distinct records>,
      "supporting_records": ["<review or match id>", "..."],
      "exceptions": ["<counterexample or scope limit>", "..."],
      "recommended_check": "<a concrete check that would address it>",
      "target_roles": ["generation" | "reflection" | "ranking" | "evolution" | "proximity"]
    }
  ],
  "methodological_gaps": ["<gap in how the work is being done>", "..."],
  "coverage_gaps": ["<research direction or evidence type not yet examined>", "..."],
  "caution": "<where applying this feedback would narrow future proposals too far>"
}

Feedback is applied selectively. Flag in `caution` any critique that, applied
universally, would collapse future proposals toward previous winners.
