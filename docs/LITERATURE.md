# Literature discovery

Search remains opt-in: network permission alone does not contact scholarly
connectors. The planner emits topical subquestions, not a new scientific
assessment. Each discovery admits at most four distinct subquestions with two
distinct query formulations each (eight queries, at most 24 connector calls
with the default three connectors). Legacy flat query plans retain their
four-query cap. An unusable plan falls back to the goal keywords and a
replication/limitations formulation. Every query is whitespace-normalized and
capped at 400 Unicode characters; existing result caps and connector pacing
still apply. The normal acquisition budget still bounds attempted full texts.

Only query text and year filters are transmitted to connectors. Subquestion
labels stay in local `SearchRecord.filters` for coverage auditing. Queries and
raw connector responses remain persisted, including partial failures.

## Identity and order

Fusion first resolves exact DOI/arXiv/PMID crosswalks transitively. A work carrying
both DOI and arXiv ID joins an arXiv-only hit even when their primary keys differ.
The strongest identifier wins (DOI, arXiv, PMID, exact title/year/author
fingerprint), with lexicographic ties. There are no fuzzy title merges.
Duplicate hits within a connector/query list retain their audit entries but do
not contribute repeated RRF votes. Metadata merging considers all hits from a
connector in rank/hash order, so sparse repeated hits do not erase fields.

The second-stage `lexical_rrf_v1` reranker uses 75% best-query exact-term overlap
in title/abstract (title weighted twice) and 25% normalized RRF. This is a
transparent lexical heuristic, not a learned semantic reranker. It can promote
a topically matched work over a popular irrelevant result; it does not claim
benchmark quality, scientific support, or independent replication.

Every newly discovered work stores `retrieval`: method, combined and lexical
scores, matched terms, lexically covered subquestions, and `why_this_source`
explanations. Both acquisition priority and the stored work list use this order;
`rrf_score` remains the original fusion score. Older records without retrieval
metadata fall back to RRF and are not assigned invented explanations.
The work body, including retrieval provenance, is exported in `works.jsonl`.
