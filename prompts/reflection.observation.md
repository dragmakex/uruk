You are an expert in scientific hypothesis evaluation. Analyse the relationship
between the hypothesis below and the observations in the supplied article.
Determine whether the hypothesis provides a novel causal explanation for the
observations, or whether they contradict it.

Goal: {goal}

{instructions}

Article:
{article}

Hypothesis:
{hypothesis}

Procedure:

1. Observation extraction: list the relevant observations. Every observation
   must carry an exact locator (page, section, figure, or table) from the
   article. If the article supplies no relevant observations, return an empty
   list — do not invent them.

2. For each observation:
   a. State whether its cause is already established.
   b. Assess whether the hypothesis could be a causal factor.
   c. Ask whether the observation would be expected even if the hypothesis were
      false. Consistency in one direction does not establish the converse: that
      the hypothesis predicts the observation does not mean the observation
      implies the hypothesis.
   d. Name the plausible alternative explanations. If one is better, the
      hypothesis is not a missing piece for that observation.

3. Summary: does the hypothesis offer a novel explanation for some subset?

4. Disproof: do any observations contradict it? A contradiction must bear on
   the claim's actual scope or a necessary assumption. Weak or off-scope
   evidence is not disproof.

Labels:
    already_explained              — consistent, but the causes are known.
    other_explanations_more_likely — could explain, but better explanations exist.
    missing_piece                  — offers a novel, plausible explanation.
    neutral                        — neither explains nor is contradicted.
    disproved                      — observations contradict the hypothesis.

If the observations are expected whether or not the hypothesis holds, and do
not contradict it, the label is neutral.

Respond with a single JSON object and no other text:

{
  "observations": [
    {
      "source_id": "<id of the article>",
      "locator": {"kind": "page|section|figure|table", "at": "<value>"},
      "observation": "<what was observed>",
      "cause_established": <bool>,
      "consistent_with_hypothesis": <bool>,
      "expected_regardless": <bool>,
      "alternative_explanations": ["..."],
      "label": "<one of the five labels>"
    }
  ],
  "summary": "<whether it explains a subset, and why>",
  "disproof": "<any contradiction, with the scope it bears on, or 'none'>",
  "label": "<overall label for the hypothesis>",
  "rationale": "<concise reasoning>",
  "uncertainty": "<what this review could not resolve>"
}
