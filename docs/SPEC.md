# Uruk — Autonomous Research Engine

**Purpose:** An autonomous research engine for any stage of research. Its output is evidence-backed findings; researchers set goals, approve side effects, and steer, but are not required to do the work.  
**Foundation:** Gottweis et al., *Towards an AI co-scientist* (2025), supplied as [`pre_coscientist.pdf`](pre_coscientist.pdf), and *Accelerating scientific discovery with Co-Scientist* (2026), supplied as [`coscientist.pdf`](coscientist.pdf).  
**Scope:** Domain-independent research reasoning, grounded in sources and connected to tools.  
**Implementation:** Rust throughout Uruk, including orchestration, agents, storage, tool adapters, CLI, and later add-ons.  
**Status:** Product specification, not an implemented system. Both Co-Scientist papers inform the research architecture; Rust-native execution is required (see §9). Published prompt examples and Uruk adaptation rules are included in §15.

This specification defines Uruk's behavior. It does not claim to reproduce Google's private implementation or to validate Uruk across every scientific discipline. Defaults identified as Uruk choices are not claims about either paper.

---

## 0. Purpose and boundaries

Uruk helps a researcher frame questions, understand prior work, generate and challenge ideas, design studies, perform authorized computational work, interpret results, and communicate evidence-backed conclusions.

A researcher can enter at **any stage**, bring existing work, request only the help they need, and stop without completing a discovery loop.

```text
Researcher: question, papers, data, ideas, constraints, requested deliverable
                                  |
                                  v
                    Supervisor + research plan
                                  |
          Generation / Reflection / Ranking / Evolution
                      Proximity / Meta-review
                                  |
                      Approved tools and methods
                                  |
              Sources, evidence, artifacts, feedback
                                  |
                    Researcher reviews and steers
```

Two operating modes share the same records and roles:

- **Task:** Complete a bounded request, such as reviewing a protocol or analyzing a dataset. Use only the necessary roles. No mandatory hypothesis generation, tournament, or iteration.
- **Campaign:** Explore an open research goal through asynchronous generation, review, comparison, evolution, and feedback. Continue within an explicit budget until a stopping condition holds.

“General-purpose” means a common research workflow and evidence model, not built-in expertise or execution capability for every field. Missing methods, data, tools, or expertise must be visible limitations.

### Non-goals

- Building a research-credit market, plugin marketplace, distributed cluster, or reusable workflow platform in v1. The optional market is a post-v1 add-on (§7.1).

## 1. What comes from the papers

The 2025 preprint and 2026 article describe the same research line: a general-purpose **structured scientific thinking and hypothesis-generation system**, validated primarily in three biomedical applications. They are not two independent validations of Uruk. Uruk extends that foundation to other research stages; those extensions must be evaluated separately.

The following architecture references are to the 2026 article; the corresponding earlier architecture is described in the 2025 preprint §§3.1–3.5.

| Paper mechanism | Uruk requirement | Source in 2026 article |
|---|---|---|
| Expert-defined goals, constraints, preferences, and feedback | Researcher controls the goal, rubric, permissions, and direction | Fig. 1; Methods, “From research goal to research plan configuration” and “Expert-in-the-loop interactions” |
| Supervisor plus six specialized agents | Preserve these responsibilities; do not add a core agent for every discipline or workflow | Methods, “Overview of Co-Scientist architecture” and “The specialized agents underpinning Co-Scientist” |
| Asynchronous workers and weighted allocation | Campaign work is selected from current research state, not a fixed pipeline | Methods, specialized-agent orchestration |
| Persistent context memory | Retain research state and feedback across tasks and restarts | Methods, architecture and research-plan configuration |
| Generate, debate, evolve | Use the discovery loop when competing ideas warrant it | Methods, Generation, Ranking, and Evolution agents |
| Elo tournament, initial rating 1,200 | Relative prioritization of comparable candidates; not truth or confidence | Methods, “Ranking agent” |
| Evolution produces new hypotheses | Preserve parents and insert linked children | Methods, “Evolution agent” |
| Proximity and meta-review | Organize related ideas and feed recurring critiques into subsequent work | Methods, Proximity and Meta-review agents |
| Tools and private research material | Ground work in literature, supplied sources, databases, and specialized tools | Methods, “Tool use in Co-Scientist” |

**Uruk extensions:** task mode, non-hypothesis deliverables, explicit evidence provenance, computational execution and result ingestion, reproducibility records, domain profiles, approval policies, Rust-native execution, and an optional later research market.

Rust is Uruk's implementation requirement, not a requirement of either paper. Neither prescribes a research-credit market or establishes autonomous laboratory validation. The 2026 Code availability section identifies Python as the implementation language and says the full source is not public. Its referenced Supplementary Notes 8 and 9 are not included in `coscientist.pdf`; however, the supplied 2025 preprint includes **eight example prompts** in Appendix A.2, Figures A.1–A.8 (pp. 40–46), reproduced in §15. These examples are not a complete system prompt library or executable implementation.

### 1.1 Additional operational guidance from the 2025 preprint

| Idea | Uruk use | Source in 2025 preprint |
|---|---|---|
| Explicit preferences, attributes, and constraints | Parse these separately into the plan; novelty is optional for tasks such as replication | §3.2; Fig. A.9 |
| Track which generation strategies work | Record Generation/Evolution strategy, parent lineage, outcomes, and cost; use this feedback in allocation | §§3.2–3.3 |
| Collaborative generation debate | Begin with distinct candidates, then critique and refine; target 3–5 contributions with a hard cap of 10 | Fig. A.2 |
| Observation-based explanatory review | Distinguish known explanations, better alternatives, possible missing explanations, neutrality, and contradiction | Fig. A.3 |
| Avoid incomparable review scores | Give pairwise judges the substantive reviews, not their independent numeric scores | Fig. A.4 |
| Bounded comparison debate | Summarize both candidates, question assumptions, and compare against the same goal; do not rely on claimed judge impartiality | Fig. A.5 |
| Feasibility and analogical divergence | Improve practical implementability or propose one genuinely different mechanism, not a collage of existing ideas | Figs. A.6–A.7 |
| Review synthesis without re-judging individuals | Produce actionable recurring critiques; apply them selectively to avoid narrowing every new idea to previous winners | §3.3.6; Fig. A.8 |
| Research roadmap and expert-facing formats | Periodically update directions, discriminating experiments, and requested proposal formats; cite suggested experts without contacting them | §3.3.6; Figs. A.20–A.22 |
| Evaluate ranking outside its own tournament | Compare to known outcomes and blinded expert judgments; Uruk additionally requires matched-cost baselines | §§4.1–4.3; Appendix A.5.2 |
| Recheck safety after generation and evolution | A safe goal can lead to an unsafe candidate or research direction; review intermediate outputs too | §§4.4, 6 |

The preprint explicitly limits its demonstrated multimodal/data integration and notes missing negative results, inaccessible literature, hallucinations, and biased auto-evaluation (§§5, 7). Accepting images, datasets, or specialized tools in Uruk does not establish that their interpretation is reliable; report modality/coverage gaps and validate each integration separately.

## 2. Research capabilities

These are entry points and deliverables, not mandatory sequential stages or additional core agents.

| Research need | Typical inputs | Expected deliverable |
|---|---|---|
| Frame a problem | Question, objectives, practical constraints | Scoped questions, operational definitions, assumptions, success criteria |
| Review literature | Papers, search terms, bibliography | Cited synthesis, evidence table, disagreements, gaps, search coverage |
| Discover hypotheses | Goal, observations, prior work | Distinct testable hypotheses, mechanisms, predictions, possible falsifiers |
| Critique existing work | Hypothesis, protocol, analysis, manuscript | Assumption checks, counterarguments, methodological defects, revision suggestions |
| Design a study | Candidate explanation, resources, constraints | Protocol, controls, measurements, analysis plan, feasibility and approval requirements |
| Develop research software | Method or analysis plan, optional repository | Reviewable code and a runnable correctness check |
| Execute computational work | Approved code/protocol, data, environment | Recorded execution, raw outputs, logs, metrics, reproducibility manifest |
| Analyze and interpret | Dataset, observations, execution results | Validated analysis, uncertainty, alternative explanations, limitations |
| Replicate or falsify | Published result, code, data, claim | Reproduction attempt, discrepancies, scope of support or contradiction |
| Synthesize and communicate | Evidence, reviewed claims, target format | Research overview, report, manuscript/grant draft, figures, next-step recommendations |

Examples of valid standalone requests:

- “Compare these five papers and explain where their measurements disagree.”
- “Review my study design for confounding; do not generate new hypotheses.”
- “Reproduce Figure 3 from this CSV and script, without network access.”
- “Find competing explanations for this anomaly and propose discriminating tests.”
- “Compare these two simulation methods against the same reference measurements.”

A literature-only request must work without a repository or execution tool. An analysis request must work without inventing a hypothesis population. A campaign can begin from researcher-supplied ideas rather than Generation.

## 3. Responsibilities and architecture

| Layer | Owns | Does not own |
|---|---|---|
| Uruk research policy | Goals, plans, agent strategies, review criteria, evidence interpretation, tournament, next-work selection | Process supervision, generic retry machinery, provider transport |
| Execution runtime | Durable tasks, concurrency, retries, waits, cancellation, execution history | Scientific validity, hypothesis quality, research priorities |
| Domain profiles and tools | Domain terminology, methods, rubrics, source/tool adapters, specialized nested teams | Core lifecycle, global permissions, budget increases |
| Research records | Versioned inputs, candidate lineage, evidence, reviews, decisions, artifacts | Hidden mutable agent memory |
| Interface | Researcher instructions, approvals, inspection, exports | Silently approving generated plans or promoting claims |

Keep the research-policy code independent of runtime-specific types. This is an ownership boundary, not a requirement to implement multiple backends or a generic runtime plugin system.

Start with one Rust Cargo package, a library and CLI binary, and ordinary modules for research policy, agents, runtime, storage, tools, and reporting. Split crates only when an actual reuse or dependency boundary needs it. All Uruk-owned implementation code is Rust; do not introduce a Node, Python, or other application service to implement a subsystem.

Researcher-supplied scripts, notebooks, existing scientific binaries, and remote model APIs may be external inputs/tools. Their adapters, permission enforcement, process supervision, and evidence ingestion remain Rust. Supporting an external research tool does not make it a second orchestration runtime.

## 4. Research records

### 4.1 Goal and plan

A goal records:

- Research question or requested task; mode (`task` or `campaign`); requested deliverables.
- Source references: files, PDFs, URLs, datasets, code, images, observations, or existing project records. A repository and source list are both optional.
- Scope, exclusions, known facts, assumptions, and practical constraints, kept distinguishable.
- Evaluation rubric and acceptance criteria appropriate to the requested work.
- Allowed tools, data-disclosure policy, execution permissions, and required human gates.
- Limits on wall time, model calls/tokens, tool execution, cost where measurable, and campaign iterations.

The Supervisor produces a versioned plan containing the selected roles, dependencies, proposed methods, research priorities, budgets, outputs, and stopping conditions. Following the 2025 preprint §3.2 and Fig. A.9, preserve **preferences** (desired properties), **attributes** (comparison axes), and **constraints** (requirements/limits) separately. The same plan revision supplies these to generation, review, comparison, and evolution. Ask for clarification only when missing information would materially change validity, scope, cost, or permissions. Otherwise record explicit assumptions.

Low-risk work within permissions already supplied by the researcher may proceed. New privileges, external side effects, or consequential experimental choices require approval. An agent cannot approve its own expansion of scope.

Goal or rubric changes create a revision. Subsequent tasks reference that revision; in-flight results retain their original context. The Supervisor must decide whether they remain applicable before reusing them.

### 4.2 Common records

All records have stable IDs, schema versions, creation provenance, and project/run identity. Large content lives in artifacts referenced by hash rather than repeated in prompts. Existing work can be explicitly referenced across runs, retaining its original provenance; context is project-scoped and never silently shared across private projects.

| Record | Required meaning |
|---|---|
| `Source` | Origin, available title/author/date/identifier, retrieval time, content hash/version, access limitations, and exact locator when cited |
| `ResearchItem` | Kind (`question`, `hypothesis`, `proposal`, `analysis`, `synthesis`, or `manuscript`), content, source/evidence links, parent IDs, author provenance |
| `Evidence` | Observation or result, method, inputs, provenance, artifacts, limitations, and the specific claim it bears on |
| `Review` | Target item version, review strategy, assessment, evidence considered, objections, unknowns, and suggested next actions |
| `Experiment` | Computational or external protocol, target claims, inputs, controls/checks, expected outcomes, analysis method, permissions, execution/result links |
| `Match` | Candidate IDs, rubric revision, evidence snapshot, comparison order, judge/model identity, outcome, rationale, and rating update |
| `Decision` | Plan change, human feedback/approval, disposition, or scheduling choice, with actor, reason, and referenced records |
| `Task` | Role/tool and strategy, versioned inputs, prompt ID/revision/content hash, model/configuration, feedback version, dependencies, priority rationale, permission envelope, cost reservation, runtime identity, attempts, and outputs |
| `Artifact` | Media type, immutable content/hash, producing task, storage reference, and access classification |

These are data contracts, not a demand for one class, service, or database per row.

**Item content is immutable.** Revision or evolution creates a new item with parent links. Reviews, observations, dispositions, and ratings are separate records, so new evidence does not rewrite what was originally proposed. Superseding an artifact or correcting evidence preserves the original and records why it was superseded.

A hypothesis additionally states its claim, explanatory mechanism, assumptions, scope, testable predictions, potential falsifiers, and proposed validation. Do not force these fields onto a bibliography or manuscript draft.

### 4.3 Evidence and epistemic status

Evidence distinguishes retrieved literature, supplied observations, computational results, external experimental results, formal verification, human assessments, and agent critiques. An LLM simulation review is reasoning, **not** an executed simulation. Several agents repeating the same source are not independent corroboration.

Each link from evidence to a claim records `supports`, `contradicts`, `inconclusive`, or `context`, with justification and limits. An evidence item may support one claim while contradicting another.

Assessments use:

- **Untested:** proposed, with no relevant validation yet.
- **Plausible:** survived review; not experimentally established.
- **Supported:** relevant evidence supports the scoped claim, with limitations stated.
- **Contested:** material supporting and contradicting evidence remain unresolved.
- **Refuted:** evidence contradicts the scoped claim or a necessary assumption.
- **Inconclusive:** available checks cannot resolve the claim.

These are revisable assessments, not a monotonic confidence ladder. Every assessment beyond Untested references a Review or Decision explaining its basis. Supported and Refuted require claim-relevant evidence beyond agent opinion. Novelty, usefulness, feasibility, and safety are separate assessments, not synonyms for truth.

Workflow disposition (`active`, `blocked`, `archived`, `duplicate`) is separate from scientific assessment. A duplicate keeps a link to its representative and all provenance. A blocked tool says nothing about whether a claim is true.

Citation requirements:

- Link each substantive sourced claim to a retrieved or supplied source and a page, section, figure, table, code location, or data slice when available.
- Preserve the distinction between a source's reported result and Uruk's inference from it.
- Do not fabricate references or imply full-text verification when only an abstract is accessible.
- Record search queries, dates, filters, and unavailable material. “Novel within searched sources” is not a universal novelty guarantee.
- Preserve contradictions, negative results, retractions/corrections when known, and incomplete coverage. Do not silently select only supportive sources.

## 5. Core roles

Retain the paper's Supervisor and six specialized roles. They are logical responsibilities with versioned strategies, not seven permanently running processes. One bounded task may use one or two roles; campaigns use the full feedback system as needed.

### Supervisor

Translate the goal into a plan, choose the next useful work, allocate resources, handle human input, and stop. Consider unanswered questions, missing reviews, uncertainty, coverage, cost, and dependencies—not only the highest Elo.

Scheduling is Uruk policy; launching, waiting, retrying, and cancellation belong to the runtime. Persist each selection and its rationale before dispatch. Persist sampled choices rather than drawing fresh randomness on resume.

### Generation

Produce questions, focus areas, hypotheses, or candidate proposals. Use the paper's strategies: literature exploration, simulated scientific debate, iterative assumption identification, and research expansion from prior feedback.

Ground suggestions in supplied sources or approved retrieval. Identify what is established, inferred, and speculative. When sources are unavailable, return provisional ideas with explicit grounding/novelty limitations.

### Reflection

Critically assess an existing item or result using the appropriate strategy:

| Strategy from the paper | Use |
|---|---|
| Initial | Cheap goal-alignment, correctness, novelty, feasibility, and safety screening |
| Full | Source-grounded review, including prior work and conflicting evidence |
| Deep verification | Decompose and independently check assumptions; distinguish fatal from repairable defects |
| Observation | Check whether the proposal explains existing observations better than alternatives |
| Simulation | Walk through the mechanism or proposed protocol; identify failure modes without claiming execution |
| Recurrent/tournament | Revisit reviews using accumulated evidence and comparison patterns |

Uruk additionally allows Reflection to request computational checks, replication, statistical diagnostics, and domain-specific experiments. It consumes the returned evidence rather than assuming a requested check succeeded.

A review returns the assessment, specific objections, supporting and contradicting evidence, unresolved issues, and bounded next actions. Researcher reviews are accepted with attribution; authority to steer does not make an empirical assertion true.

For Deep verification, extract assumptions and sub-assumptions, then assess them in focused contexts without presenting the desired conclusion as a fact (2025 §3.3.2; Figs. A.14–A.15). Recombine the results to distinguish a fatal premise from a repairable detail. Separate model contexts are intended to reduce anchoring; they do not make agent judgments independent empirical evidence.

Observation review also records the source prompt's explanatory label: `already_explained`, `other_explanations_more_likely`, `missing_piece`, `neutral`, or `disproved` (2025 Fig. A.3). These labels are **not** replacements for §4.3 assessments. `missing_piece` means a plausible explanatory contribution, not Supported. An observation expected whether or not the hypothesis holds is neutral. A `disproved` label is a proposed contradiction requiring evidence/applicability checks before Refuted is recorded. Allow an empty observation set when the article supplies no relevant observations.

### Ranking

Compare competing candidates against the goal's rubric. Use a tournament only when there are at least two meaningfully comparable candidates and prioritization is useful. A standalone analysis or report need not acquire an Elo rating. See §7.

### Evolution

Create new versions or candidates through grounding, coherence/feasibility improvements, inspiration, combination, simplification, and out-of-box thinking. These are the paper's strategies. Uruk also explicitly preserves productive follow-ups from partial or negative results.

Never edit a ranked parent in place. Children state what changed, why, and which assumptions/evidence were inherited or rejected. They require their own review; they do not inherit a parent's support or rating.

### Proximity

Find related ideas, comparison pairs, clusters, likely duplicates, and unexplored areas. Start with structured overlap and a bounded semantic comparison; use embeddings only when scale warrants them.

Similarity alone must not delete or invalidate a candidate. Check meaningful differences in mechanism, scope, predicted outcomes, and proposed tests before marking it a duplicate. Preserve minority and contradictory ideas.

### Meta-review

Synthesize recurring weaknesses, methodological gaps, useful comparison patterns, and coverage gaps into versioned feedback for subsequent work. Produce the research overview and requested evidence-grounded reports or drafts.

Feedback is linked to its supporting records and supplied to relevant roles. Avoid turning one judge's preference into a universal research rule. As in the 2025 §3.3.6 strategy, Generation applies critiques selectively rather than overfitting every proposal to prior reviews. This feedback changes subsequent context/prompts, not model weights. Context summaries are navigational aids, not replacements for original evidence.

Periodically produce an overview of research directions, their importance, unresolved questions, illustrative candidates, and discriminating experiments; feed it back to Generation, not only to the final report. Requested grant/Specific Aims-style formats are output views over the same evidence, not permission to invent missing studies. Meta-review may identify possible expert reviewers from cited relevant work, but never contacts them without authorization.

### Tool access

Generation, Reflection, and Evolution may request approved tools; Supervisor may schedule explicit tool tasks. Ranking and Proximity consume versioned research records. Meta-review synthesizes those records and can request missing evidence through Supervisor.

A domain expert team is a tool/workflow below these roles, not another set of core agents. Tool access remains bounded by the task's permissions regardless of the requesting role.

### 5.1 Prompt contracts

§15 contains all eight prompt examples from the 2025 Appendix A.2, with source provenance and mandatory Uruk adaptations. They are strategies for the existing roles, not new agents. The preprint does not provide example prompts for every role/review strategy; do not label Uruk-authored Supervisor, Proximity, or other missing templates as published prompts.

Implement prompts as versioned text assets loaded by Rust, not a new scripting runtime. Persist the selected strategy, source figure (when applicable), template revision/hash, rendered-input reference, model settings, and feedback version on each task. Missing inputs, unresolved template variables, or invalid structured outputs fail validation before a scientific record is accepted. Prompt revisions create new tasks; resume must not reinterpret a completed output under a changed prompt.

Keep the hypothesis prompts specific to discovery/proposal work. Literature-only, critique-only, analysis, and writing tasks use the relevant §5 role and their own typed deliverable contract; do not create hypotheses merely to fit a published prompt.

## 6. Execution and research lifecycle

### Task mode

```text
Interpret request → select necessary work → run/review → deliver artifact + limitations
```

Examples: Reflection can critique a supplied protocol directly; Meta-review can synthesize a supplied evidence set; an approved analysis can execute and then undergo Reflection. No discovery loop is inserted merely to exercise all roles.

### Campaign mode

The conceptual loop is:

```text
Ground / generate → reflect → compare → evolve → meta-review → next research work
                         ↘ tools / experiments ↗
```

This is not a global stage barrier. Separate items can be generated, reviewed, tested, or ranked concurrently. Dependencies are local: review needs its target, analysis needs its data, comparison needs reviewed candidates, and an experiment needs its approved protocol.

Supervisor selects eligible work using non-negative weights informed by missing reviews, new evidence, uncertain assumptions, tournament coverage, underexplored clusters, and estimated information gain per cost. Reserve some exploration capacity so low-ranked or new directions cannot starve. Numerical weights are Uruk configuration, not paper-prescribed constants.

Following 2025 §§3.2–3.3, persist periodic summary snapshots: candidates generated/awaiting review, review and match coverage, cluster coverage, task failures, resource use, and outcomes by Generation/Evolution strategy. Track novelty/feasibility objections and external-evidence results separately from debate wins. Use these summaries to adjust work weights and identify diminishing returns; do not optimize the scheduler solely for increasing Elo.

The published Generation and Ranking debate prompts (Figs. A.2, A.5) target 3–5 conversational turns, capped at 10. Uruk counts a turn as one model contribution and enforces the cap in Rust, with stricter task/run budgets taking precedence; tool calls are metered separately. A debate can finish earlier or return partial/insufficient-basis output at the cap. Do not start an unbudgeted eleventh call just to force a conclusion.

The runtime enforces bounded concurrency and tool-specific limits. Separate task workspaces prevent concurrent writers from modifying the same research code or artifacts. Scientific records are committed through one controlled write path; model agents return proposed records rather than updating shared state directly.

### Human interaction

At any point, the researcher can add sources, seed ideas, provide reviews, change constraints, request a particular analysis, approve/deny work, or stop. Persist that input before acting on it. Plan revisions must invalidate stale approvals and refresh comparisons if the rubric changes.

External experiments may take days or weeks: export the protocol, park the dependent work, and accept results later with provenance. Do not leave a worker or model conversation running while waiting for a person. Independent permitted work may continue.

### Budgets, failure, and stopping

- Enforce a finite deadline and task/iteration limits even when provider cost is unknown. Report unknown cost instead of recording zero.
- Reserve budget for in-flight work and final output before admitting more tasks. Nested tool teams share the parent budget; they cannot mint a new one.
- Retries are bounded and charged. Retry infrastructure failures when safe; do not retry negative scientific results until they look positive.
- Mark unavailable optional capabilities and choose a permitted alternative, or return a blocked/partial result. Never imply the missing check passed.
- Stop when the deliverable is satisfied, limits are reached, permitted work is exhausted, progress stalls under the plan's criterion, the researcher cancels, or a safety boundary is violated.
- Cancellation stops admission, requests cancellation of attached work, and terminates owned subprocesses after a bounded grace period. Late results retain their cancellation context.
- Always preserve completed evidence and generate a status/partial-output manifest without needing another paid model call. Final Meta-review runs only if permitted and budgeted; report when it was skipped.

Run states distinguish `running`, `waiting-for-human`, `blocked`, `completed`, `budget-exhausted`, `cancelled`, and `failed`. A scientifically negative result can be a successfully completed task.

## 7. Evaluation and tournament

The default discovery rubric follows the paper: goal alignment, plausibility/correctness, novelty, testability, and safety. Add domain-appropriate criteria such as explanatory power, feasibility, expected impact, measurement validity, reproducibility, and cost.

Do not apply novelty as a requirement to replication, or clinical feasibility to a mathematical conjecture. Unsafe or out-of-scope candidates cannot offset a failed gate by scoring well elsewhere.

For a tournament:

1. Compare candidates serving the same goal and rubric revision; explain what is being prioritized.
2. Supply the same relevant evidence snapshot and budget to both sides. Hide author/model identity where practical and randomize presentation order. Following 2025 Fig. A.4, omit independent reviewers' numeric scores from the judge's input because they may not be comparable; retain their evidence and substantive criticisms. Also withhold current Elo/credit balances to avoid circular judgments. Preserve the original reviews in storage.
3. Prefer related pairs, newer items, and promising candidates. Preserve some cross-cluster comparisons rather than creating disconnected leaderboards.
4. Use multi-turn scientific debate for consequential/top-ranked comparisons and cheaper single-turn comparisons elsewhere.
5. Return a winner, draw, or insufficient-basis outcome, with explicit evidence and tradeoffs. Insufficient basis triggers more work or remains unresolved; it is not a fabricated win.
6. Persist the comparison and apply its rating update once, in a recorded order.

Use classic Elo with initial rating **1,200** from the paper. `K = 32` is a Uruk starting choice, not a paper attribution. Wins score 1, draws 0.5, losses 0; insufficient-basis comparisons do not update ratings. Display match counts and relevant evidence status beside ratings.

Elo is relative prioritization within a comparison cohort. It is not probability, empirical confidence, severity, or proof of discovery. It must not change because money was spent or receive arbitrary “evidence bonuses”; evidence affects the reviewed assessment and comparison itself.

Relevant contradictory evidence cannot be ignored because a candidate argues well. An execution result must first pass method, provenance, and applicability checks; a deterministic test of the wrong thing is not stronger evidence. Incomparable tradeoffs remain explicit rather than forcing a universal scalar ranking.

New evidence makes dependent assessments/comparisons candidates for re-review. Retain historical ratings with their evidence snapshot and visibly mark stale rankings until refreshed. Rubric revisions start a new cohort at the initial rating; never mix ratings across changed objectives.

### 7.1 Optional later add-on: research market

**Post-v1, opt-in, implemented in Rust, disabled by default.** The baseline tournament and Supervisor must work without it. Do not scaffold a market service or ledger until this add-on is implemented. It is a Uruk experiment in resource allocation, not a mechanism attributed to the paper.

The market allocates scarce research effort among competing candidates, clusters, and proposed tests. It can later support virtual stakes on explicit, verifiable outcomes.

| Quantity | Meaning | Authority |
|---|---|---|
| Elo | Relative quality under a versioned comparison rubric | Ranking; updated only by matches |
| Research credits | Internal allocation of permission to spend research effort | A fixed run pool and recorded allocation/transfer rules |
| Actual resource budget | Tokens, calls, compute time, wall time, and monetary cost | Researcher-approved hard limits, enforced by the runtime |

Credits are virtual accounting units, not money, tokens for sale, or a claim on real funds. Credit transfers never increase the actual budget. Wealth, bids, stakes, or credits spent never directly change Elo or scientific assessment.

**First market increment — effort allocation:**

- Supervisor distributes a fixed credit pool across research directions using a versioned policy informed by uncertainty, coverage, expected information gain, and tournament results. Reserve an exploration share so established winners cannot monopolize research.
- An agent may propose a bid for a concrete review, experiment, or comparison, naming the target, expected information, evidence gap, and maximum credit cost. Supervisor validates it against permissions, dependencies, and the real remaining budget.
- Reserve credits atomically before dispatch, settle consumed effort once, and release only the unused reservation on completion/cancellation. Failed work still costs what it consumed. Retry attempts must reserve their own incremental cost; replaying a settled attempt cannot charge it again.
- Archived candidates return uncommitted balances to the pool. New or evolved candidates receive explicitly allocated credits, not freshly minted balances; creating duplicates or new agent identities cannot create funds.
- Persist allocation, bid, reservation, spend, release, and transfer records in the existing SQLite store. Use non-negative integer units, checked arithmetic, unique settlement IDs, and a balanced ledger: available balances plus reserved/escrowed credits plus spent credits equal the issued pool. Do not add a separate database or scheduler.

**Later market increment — optional outcome staking:**

A stake locks credits on a precisely defined outcome with a target item version, resolution deadline, validation method, and designated independent reviewer. Settlement must reference qualifying evidence and the recorded resolution, not just an Elo win, agent vote, or mutable `Supported` label. A bettor must not adjudicate its own payout. Resolve genuinely unknown/disputed outcomes as unresolved and return escrow under the predeclared rules; absence of a result is not a loss. Payouts redistribute escrow without minting credits, and each stake settles at most once. Revising the underlying claim does not transfer the bet to its child.

Market-enabled reports show credits allocated/spent and settlement evidence separately from Elo and scientific status. Before enabling the add-on, test conservation, concurrent reservations, insufficient balances, cancellation/refunds, duplicate settlement, and independence of Elo. Compare useful evidence per unit cost against the baseline Supervisor; the market stays optional if it adds complexity without improving research.

## 8. Tools, experiments, and reproducibility

Tools may retrieve/read sources, query databases, run calculations, inspect code, execute analyses or simulations, invoke formal checkers, or call specialized models. Use existing tools and adapters before creating domain frameworks.

Each tool declares input/output schemas, version, required capabilities, side effects, resource limits, and replay/retry safety. Its result is structured evidence plus artifact references, not an unexamined prose verdict.

Every executed computation records:

- Exact input data/source versions and hashes, code revision or code artifact, tool/environment versions, parameters, and seeds where applicable.
- Commands or equivalent API requests, excluding secrets; start/end times, exit status, stdout/stderr references, resource use, and failure/coverage information.
- Raw outputs, transformations, analysis code, and a rerun command or explicit reasons why reproduction is unavailable.

Generated code runs in a bounded workspace with a runnable check appropriate to its purpose. Never overwrite raw data. Missing, malformed, or partial outputs fail validation rather than being promoted to observations.

Study/analysis plans should state the question, primary outcomes, assumptions, controls/baselines, sample/measurement units, relevant randomization/blinding, missing-data handling, uncertainty estimates, and known confounders. Include power/sample-size reasoning and multiple-testing treatment where applicable; mark inapplicable fields instead of inventing precision.

Separate exploratory from confirmatory analysis. Record post-result changes to a protocol or analysis plan; never describe them as pre-specified. Correlation is not causation, non-significance is not proof of no effect, and absence of a discovered counterexample is not proof of correctness.

An external experimental result records its contributor, protocol version, collection context, raw-data availability, and review status. Importing it is not independently reproducing it. Domain-specific validation standards govern support; there is no universal rule that code execution outranks every other evidence type.

## 9. Rust-native runtime

**Decision: implement Uruk entirely in Rust, using Tokio for async execution and SQLite for durable state.** Build the minimum application-specific runtime required by §6, not a general workflow engine. Tokio supplies concurrency, not durability; Uruk must implement and test the persistence/recovery guarantees below.

### 9.1 Implementation boundaries

| Concern | Rust implementation |
|---|---|
| Research policy and agents | Typed structs/enums, versioned prompts, validated model outputs; no model-generated mutation of shared state |
| Concurrent work | Tokio tasks with owned task handles, bounded admission, and per-tool concurrency limits |
| Durable state | SQLite through `sqlx`, transactional task/research records, schema migrations, unique IDs |
| Models and retrieval | Rust HTTP/provider adapters, bounded retries/timeouts, cancellation, usage accounting |
| External tools | Rust process/API adapters, isolated workspaces, explicit permissions, bounded output capture |
| CLI and reporting | Rust command handling and deterministic exports; reports do not require a model call |
| Optional market | A later Rust module using the same task admission path and SQLite transactions |

Use `serde` for persisted and provider-facing data, but also validate semantic constraints at trust boundaries. Use typed `Result` errors; no panic/`unwrap` on I/O, model, or tool failure paths. Keep blocking/CPU-heavy work off async executor threads, and do not hold state locks across model or subprocess waits. Pin the Rust toolchain and commit `Cargo.lock`.

### 9.2 Persistence and recovery

- One scheduler process owns a project at a time, enforced by an OS-held project lock. Other CLI invocations may inspect state or submit durable control requests; they must not start a competing scheduler. This is a single-machine v1 design, not a distributed execution guarantee.
- Store research records, task attempts, approvals, budget reservations, and scheduling decisions in `.uruk/state.sqlite`. The persisted task set is the queue's source of truth; in-memory ready lists are rebuildable. No message broker or second writable research store.
- Persist task identity, versioned inputs, permission envelope, selected strategy, and budget reservation before dispatch. Record nondeterministic scheduling choices rather than drawing new ones during recovery.
- On completion, validate outputs and commit the task outcome, referenced research records, budget settlement, and any applicable rating update atomically. Unique task/match/settlement keys make duplicate result delivery harmless. Short serialized transactions must not span external calls.
- Write and durably finalize immutable artifacts before committing references to them. A crash may leave an unreferenced artifact, but must not leave a successful task pointing to an incomplete file. JSONL/Markdown exports are derived snapshots, not mutable authority.
- Resume the same run and task identities. Reuse committed results, restore pending approvals, and rebuild eligible work from recorded dependencies. Reconcile unfinished attempts and owned subprocesses before resuming execution.
- Local state application is idempotent; external effects are **not exactly-once**. An action may have succeeded before the process lost its result. Reconcile through the provider/tool's request identity when possible; otherwise mark it uncertain and require review before retrying an unsafe action. Preserve possible incurred cost rather than assuming it was free.
- Approval waits are persisted records, not live worker threads. Decisions target the exact request payload/version and survive restart. A changed request needs a new approval.

Context summaries and rankings are derived from durable scientific records; they are not a separate agent-memory database. Reusable extraction may be cached by source hash and tool version. Distinct stochastic trials need distinct task identities/seeds and must not collapse into one cached result or count reused results as independent replications.

### 9.3 Cancellation and runtime checks

Cancellation stops admission, persists the request, propagates through child tasks, interrupts outstanding model requests where possible, and terminates owned subprocess trees after a grace period. Dropping a Rust future is not sufficient process cleanup. Tool adapters must enforce OS-level containment and recover process ownership safely after a crash; unsupported containment is an unavailable execution capability, not permission to run uncontained.

Before adding live model calls, Rust integration tests with temporary SQLite databases and mock tools must demonstrate:

1. Concurrent reviews obey global/tool limits while unrelated work progresses independently.
2. Completed evidence selects new tasks and bounded iterations, rather than only traversing a predeclared static pipeline.
3. Kill/restart at dispatch and completion boundaries without duplicate research records, budget settlement, or Elo updates; uncertain external outcomes are surfaced.
4. Approval/external-data waits survive restart, accept only the pending version, and respect denial.
5. Cancellation reaches child computations and owned processes, retaining labelled partial outputs.
6. A report and evidence manifest can be exported from persisted state without a provider or external workflow service.

## 10. Domain profiles

A profile supplies terminology, example prompts, review rubrics, source mappings, tool configuration, and domain-specific safety/validation rules. Start with plain configuration and existing adapters, not a plugin SDK. Profiles may narrow permissions, never widen the researcher's grants.

Examples, not a promise to ship every integration in v1:

| Profile | Domain tools/evidence | Key review concerns |
|---|---|---|
| General research | Supplied documents, approved search/retrieval | Source quality, contradictory claims, coverage, traceability |
| Computational research | Rust computations, datasets, supplied notebooks, external simulation or theorem-checking tools | Assumptions, controls, numerical/statistical correctness, reproducibility |
| Experimental science | Literature/databases, protocol design, externally supplied results | Feasibility, ethics, controls, uncertainty, independent validation |

Profiles are optional configuration over the same Rust implementation. A general-research run must not require specialized scientific binaries or a code checkout.

## 11. Researcher interface and outputs

Begin with a local CLI and file-based inputs/outputs. A chat or graphical interface may later expose the same operations; it is not a v1 prerequisite.

Proposed interface for the Rust `uruk` binary, **not yet implemented**:

```text
uruk run --goal <text> --mode task|campaign [--input <path-or-url> ...] [--profile <name>]
uruk resume --run-id <id>
uruk status --run-id <id>
uruk feedback --run-id <id> --file <path>
uruk approve --run-id <id> --request-id <id>
uruk deny --run-id <id> --request-id <id>
uruk stop --run-id <id>
uruk report --run-id <id>
```

Store the normalized goal, effective configuration, permissions, budgets, model/tool/prompt versions, and input hashes for every run. Approval references an immutable request identifying the exact action, inputs, scope, and limits. Changing that payload requires a new approval.

`status` shows the current goal revision, completed/running/blocked work, human requests, resource use, evidence changes, and leading candidates where applicable. No finding, null result, and no available tool are distinct outcomes. “No discovery” is not an execution error.

A run exports only applicable artifacts under `runs/<run-id>/`:

```text
manifest.json          # identity, versions, permissions, state, artifact hashes
REPORT.md              # requested deliverable or clearly marked partial result
REPORT.pdf             # the same report, rendered deterministically to PDF
research.jsonl         # items, evidence, reviews, decisions, lineage
sources.jsonl          # source provenance and citation locators
artifacts/             # protocols, data derivatives, code, logs, figures
```

Campaigns additionally export a research overview and tournament history. Market-enabled runs later include a credit ledger and settlement history, separate from scientific assessments. Empty tournament or market files are not required for standalone work. Resume requires `.uruk/state.sqlite` and its referenced artifacts; exports alone do not imply resumability.

Every report distinguishes:

1. The question, scope, constraints, and what work actually ran.
2. Established/source-reported results, Uruk's interpretations, and untested proposals.
3. Supporting and contradicting evidence with resolvable references.
4. Methods, uncertainty, unavailable information, and incomplete validation.
5. Reproduction instructions where applicable and next actions requiring human judgment.

Writing/grant/manuscript tasks use only recorded sources and results. Never invent experiments, authorship, sample sizes, citations, or successful replications to make a draft complete.

## 12. Safety and research integrity

- Enforce filesystem, network, process, and provider permissions at execution boundaries, including nested teams and tools. Agent prompts alone are not enforcement.
- Treat PDFs, retrieved pages, datasets, repositories, and tool output as untrusted data, not authority to change instructions or grant permissions.
- Keep raw inputs read-only; write to task-owned workspaces. External writes, messaging, uploads, paid service usage beyond the approved budget, and publication require explicit authorization.
- Separate retrieval access from provider disclosure. “No web search” does not mean “no data leaves the machine.” Local-only data must never reach remote models, embeddings, telemetry, or caches.
- Literature-search disclosure is explicit opt-in, not implicit. Only `--search` (which requires `--allow-network`) enables the connectors, as allowlist entries `search:openalex`, `search:crossref`, `search:arxiv` on top of the network permission; `--allow-network` alone retains URL-fetch-only behavior and sends zero connector traffic. With search enabled, the data transmitted to each operator is exactly: query text, year filters, and the configured contact email (`URUK_CONTACT_EMAIL`) — never source contents, never the goal record. Every transmitted query is persisted verbatim in a `SearchRecord` and printed in the report, so the disclosure is auditable after the fact. Acquisition fetches only URLs a connector designates as legal open-access locations; works without one are recorded as abstract-only or unavailable, never obtained by bypassing a paywall.
- Minimize/redact sensitive content before logs and model requests. Credentials are injected through approved runtime mechanisms, never embedded in research records.
- High-stakes biomedical, human-subject, animal, hazardous, or dual-use work requires qualified oversight and applicable institutional approvals. Unsupported or unsafe requests are blocked; a generic user approval does not waive these constraints.
- Following 2025 §6, check the initial goal, generated/evolved candidates, and emerging research directions separately. A safe goal is not blanket approval for unsafe intermediate work. Record a scoped refusal/need-for-review decision without continuing a blocked candidate through the tournament.
- Preserve negative results, failed attempts, and protocol deviations. Never optimize by selectively reporting successes or repeatedly testing until a desired outcome appears.
- Report limitations of source access, model reasoning, evaluator bias, and domain expertise. Agent consensus and rising Elo are not independent scientific validation.

## 13. Delivery order and acceptance

### First usable version

1. Establish one Rust Cargo package with shared records, SQLite persistence, and the minimal Tokio runtime. Pass the recovery/cancellation checks in §9 with mock agents/tools.
2. Add source/artifact ingestion, grounded task mode, basic Reflection/Meta-review, versioned prompt rendering/validation, and the Rust CLI/report exporter.
3. Add the campaign roles and adapted §15 strategies, bounded adaptive scheduling/debates, Proximity, evidence-aware comparisons, immutable Evolution, and feedback.
4. Add an approved local computational tool and reproducibility/result ingestion. External experimental results can be supplied manually.
5. Add literature search (implemented, opt-in via `--search`): local FTS5 passage retrieval over every ingested text artifact, federated discovery against the free scholarly APIs (OpenAlex, Crossref, arXiv), RRF fusion with DOI/arXiv/PMID/title-fingerprint dedup, and open-access acquisition through the ordinary ingestion pipeline, resolved locally from discovery data (arXiv PDF → OpenAlex `best_oa_location`/`oa_url`), with the audit trail (verbatim queries, per-connector ranks and raw-response hashes in each work record, unavailable works) persisted and reported. Unpaywall and PubMed/PMC are not implemented.

Start with supplied documents plus one retrieval route and one configured model provider. Keep model selection outside research policy; add providers and specialized integrations when required. No mandatory market, vector database, hosted UI, laboratory connector, or general-purpose workflow framework.

**After v1:** optionally implement the Rust research-market add-on in §7.1, first effort allocation and only then outcome staking. Neither is a prerequisite for the baseline research system.

### Runnable acceptance checks

Use Rust unit/integration tests with small deterministic fixtures, temporary SQLite stores, and mock providers. No paid LLM, live network, or laboratory is required for CI. Run `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked` once the implementation exists.

| Scenario | Acceptance |
|---|---|
| Literature-only task | Given local papers and no repo or execution tools, produce a cited comparison without generating a tournament or claiming access to unavailable text |
| Critique-only task | Review a supplied protocol without requiring Generation; distinguish reviewer concerns from observed evidence |
| Prompt contracts | All eight published examples have source mappings; adapted templates bind required fields, validate output types, and retain prompt/model/input provenance |
| Observation review | A fixture with non-discriminating observations returns neutral; a missing-piece label alone cannot become Supported; proposed disproof requires claim-relevant evidence |
| Bounded debate | Generation and Ranking stop at the configured cap (never more than 10 model contributions); incomplete discussion does not fabricate a final winner |
| Judge inputs | Candidate presentation order can be reversed; substantive reviews remain while independent numeric scores, Elo, and credits are absent |
| Intermediate safety | A permitted goal cannot admit a blocked generated/evolved candidate; later directions are checked rather than inheriting blanket approval |
| Computational task | Execute a permitted analysis on a tiny dataset, record input/code hashes and actual outputs, provide a rerun check; failed execution cannot become support |
| Discovery campaign | Seed multiple ideas, review/compare them, create a child without mutating its parent, and carry a recorded meta-critique into later work |
| Evidence integrity | New contradictory evidence causes re-review/stale-ranking notices; duplicate citations and agent votes cannot become independent corroboration |
| Tournament integrity | Elo updates once per match, draws behave correctly, insufficient evidence causes no update, and rubric revisions do not share a cohort |
| Researcher intervention | Human ideas and feedback survive restart; approval/denial applies only to the exact request; changed inputs cannot reuse approval |
| Runtime safety | Concurrency/budget limits hold, cancellation reaches children, denied tools cannot execute, uncertain side effects are not blindly retried |
| Domain independence | A literature task and a computational fixture use the same core records; a missing scientific tool is reported as unavailable, not a scientific result |
| Rust implementation | Build and run the complete application without a Node/Python orchestration service; external research tools are optional capabilities |
| Partial completion | Budget exhaustion or a crash preserves completed evidence and permits a clearly labelled report without another model call |

These establish product behavior, not scientific usefulness. Evaluate usefulness separately on researcher-selected tasks using blinded expert review, citation correctness, method checks, held-out reproduction results where available, and time/cost to a useful deliverable. Include failures and negative results. Improvement in self-judged Elo alone is insufficient.

The 2025 §§4.1–4.3 evaluations motivate checking whether rankings agree with known outcomes and expert preferences, not interpreting Elo as a calibrated probability. Compare on the same goals with a matched-cost single-agent or non-evolving baseline; report task difficulty, judge identity, and uncertainty. Track strategy contribution and quality versus compute without treating later time buckets as independently validated discoveries. The paper's results are not Uruk acceptance thresholds.

For proposal outputs, adapt the pilot rubric in 2025 Appendix A.5.2 to the field: significance/unmet need, evidence-grounded rationale, no over-extrapolation, clear aims and methods, measurable endpoints, feasibility, factual accuracy, explicit assumptions, specificity, and clarity. Retain criterion-level findings and supporting citations rather than only a total score. The source rubric is exploratory, not a validated universal measurement instrument.

## 14. References and authority

### Research basis

- **2025:** Gottweis et al., [*Towards an AI co-scientist*](https://arxiv.org/abs/2502.18864v1), arXiv:2502.18864v1, 26 February 2025; local source: [`pre_coscientist.pdf`](pre_coscientist.pdf). Architecture: §§3.1–3.5; evaluation: §§4.1–4.4; limitations/safety: §§5–7; prompt examples: Appendix A.2, Figs. A.1–A.8, pp. 40–46; example plan: Fig. A.9; proposal rubric: Appendix A.5.2.
- 2025 PDF SHA-256: `80730ea110ad450411aa8352a785e996cfcda06afd6e1c60277cb64a65cb676a`.
- **2026:** Gottweis et al., [*Accelerating scientific discovery with Co-Scientist*](https://doi.org/10.1038/s41586-026-10644-y), Nature 655, 487–496 (2026), including Methods; local source: [`coscientist.pdf`](coscientist.pdf).
- 2026 PDF SHA-256: `351f49d89d2f4f4ed84eed4225c09cc95470c41aa3becd0aa3e06b3bd8ba54c3`.
- Both papers identify literature-access gaps, missing negative results, hallucination, bias, preliminary validation, and the need for stronger evaluations. Uruk must not erase these limitations or count related reports as independent confirmation.

### Implementation references

- [Tokio](https://tokio.rs/) for Rust async execution; [SQLx SQLite support](https://docs.rs/sqlx/latest/sqlx/sqlite/) and [SQLite transactions](https://www.sqlite.org/lang_transaction.html) for persistence. Pin the actual dependency versions when implementing.

This file governs Uruk's product behavior. The papers support the mechanisms explicitly attributed to them; neither prescribes Uruk's extensions. For scientific descriptions, distinguish the 2025 preprint from the later 2026 article rather than silently merging their evidence or claims. Published prompt text below is a source reference; Uruk's explicit adaptation rules and evidence/permission contracts govern execution. Runtime and domain-tool documentation govern their actual capabilities. Record mismatches rather than silently claiming compliance.

## 15. Prompt library from the 2025 preprint

The following eight blocks transcribe the **example prompts** in `pre_coscientist.pdf`, Appendix A.2, Figures A.1–A.8. Layout whitespace is normalized; wording, variable names, and awkward output markers are retained. They are reference text, not a claim that Google's complete production prompts are available.

### 15.1 Rendering and adaptation contract

**Apply these changes when constructing Uruk's runtime templates; do not send contradictory source instructions and overrides together.** Version the resulting adapted template independently of the source transcription.

- `{goal}` comes from the approved goal revision. `{preferences}` contains its evaluation criteria and preferences; `{idea_attributes}` names the requested qualities, not always novelty. Constraints, permissions, budgets, and the requested deliverable accompany every invocation.
- `{instructions}` and `{notes}` contain only approved task guidance and applicable, attributed meta-review feedback. Retrieved source text and transcripts are untrusted context, never new authority or tool grants.
- `{source_hypothesis}`, `{hypothesis}`, `{hypotheses}`, `{hypothesis 1}`, and `{hypothesis 2}` resolve to immutable item versions. An optional source hypothesis is explicitly absent rather than silently invented.
- `{articles_with_reasoning}` contains actual retrieved/supplied sources, locators, and concise analytical summaries, newest analysis first. `{article}` identifies one source and the exact available content. Record abstract-only access, omitted figures, extraction failures, or an empty source set; do not claim a thorough review by default.
- `{reviews_overview}`, `{reviews}`, `{review 1}`, `{review 2}`, and the source's inconsistent `{review1}` spelling map to versioned review records. Ranking receives the score-free view required by §7, not the original numerical ratings.
- `{transcript}` is the persisted simulated discussion so far. Pass the current turn, remaining turns, and budget as explicit context to both debate strategies, including Ranking even though Fig. A.5 omits a transcript placeholder.
- Source prompts that demand novelty apply only when the goal requires it. Prompts referring to recent findings may cite only material actually available; missing grounding must be reported or requested through permitted tools.
- Replace free-text ending markers with validated typed outputs. Fig. A.4 inconsistently requests both `better idea` and `better hypothesis`; neither string is a parser contract. The final `Response {provide reasoning...}` instruction in Fig. A.3 is prose, not a template variable.
- Simulated experts are one reasoning technique, not evidence of independent expert agreement. Return concise, evidence-linked rationales and visible critiques; do not require access to a model's private internal reasoning.

| Template ID | Source | Uruk result contract |
|---|---|---|
| `generation.literature` | Fig. A.1 | Proposed `ResearchItem`: claim, mechanism, assumptions, predictions, falsifiers, validation plan, source links, limits |
| `generation.debate` | Fig. A.2 | Persisted contribution plus `continue`/`final`/`partial`; final item is self-contained and linked to the discussion |
| `reflection.observation` | Fig. A.3 | `Review`: located observations, known/alternative explanations, evidence relations, explanatory label, scope/uncertainty |
| `ranking.pairwise` | Fig. A.4 | `Match` judgment: winner item ID, draw, or insufficient basis; rubric/evidence references and concise rationale |
| `ranking.debate` | Fig. A.5 | Persisted contributions followed by the same typed judgment, or incomplete/insufficient-basis result |
| `evolution.feasibility` | Fig. A.6 | New child item: parent IDs, practical changes, retained/rejected assumptions, evidence and validation plan |
| `evolution.analogy` | Fig. A.7 | One new child item: parent IDs, analogy, genuinely new mechanism, discriminating predictions and limits |
| `meta.review` | Fig. A.8 | Recorded meta-review `Decision`: recurring critiques, supporting record IDs, exceptions, recommended checks, target roles |

The Rust runtime assigns IDs and validates records; a model proposes content, not direct database writes. Debate contributions are artifacts, not empirical Evidence. Missing required inputs cause a structured blocked/incomplete outcome. Unsafe or out-of-scope work follows §12 rather than attempting to satisfy a source prompt at any cost.

### 15.2 Generation after literature review

**Source:** Appendix A.2.1, Fig. A.1, p. 40. **Template:** `generation.literature`.

```text
You are an expert tasked with formulating a novel and robust hypothesis to address
the following objective.

Describe the proposed hypothesis in detail, including specific entities, mechanisms,
and anticipated outcomes.

This description is intended for an audience of domain experts.

You have conducted a thorough review of relevant literature and developed a logical framework
for addressing the objective. The articles consulted, along with your analytical reasoning,
are provided below.

Goal: {goal}

Criteria for a strong hypothesis:
{preferences}

Existing hypothesis (if applicable):
{source_hypothesis}

{instructions}

Literature review and analytical rationale (chronologically ordered, beginning
with the most recent analysis):

{articles_with_reasoning}

Proposed hypothesis (detailed description for domain experts):
```

**Uruk adaptation:** Replace the assertion of a thorough review with a factual description of the supplied source coverage. Keep mechanism and predicted outcomes explicit; add falsifiers and a feasible validation plan. If grounding is missing, request retrieval or label the proposal provisional. This template is not a literature-summary task disguised as hypothesis generation.

### 15.3 Generation through simulated scientific debate

**Source:** Appendix A.2.1, Fig. A.2, p. 41. **Template:** `generation.debate`.

```text
You are an expert participating in a collaborative discourse concerning the generation
of a {idea_attributes} hypothesis. You will engage in a simulated discussion with other experts.
The overarching objective of this discourse is to collaboratively develop a novel
and robust {idea_attributes} hypothesis.

Goal: {goal}

Criteria for a high-quality hypothesis:
{preferences}

Instructions:
{instructions}

Review Overview:
{reviews_overview}

Procedure:

Initial contribution (if initiating the discussion):
    Propose three distinct {idea_attributes} hypotheses.

Subsequent contributions (continuing the discussion):
    * Pose clarifying questions if ambiguities or uncertainties arise.
    * Critically evaluate the hypotheses proposed thus far, addressing the following aspects:
        - Adherence to {idea_attributes} criteria.
        - Utility and practicality.
        - Level of detail and specificity.
    * Identify any weaknesses or potential limitations.
    * Propose concrete improvements and refinements to address identified weaknesses.
    * Conclude your response with a refined iteration of the hypothesis.

General guidelines:
    * Exhibit boldness and creativity in your contributions.
    * Maintain a helpful and collaborative approach.
    * Prioritize the generation of a high-quality {idea_attributes} hypothesis.

Termination condition:
    When sufficient discussion has transpired (typically 3-5 conversational turns,
    with a maximum of 10 turns) and all relevant questions and points have been
    thoroughly addressed and clarified, conclude the process by writing "HYPOTHESIS"
    (in all capital letters) followed by a concise and self-contained exposition of the finalized idea.

#BEGIN TRANSCRIPT#
{transcript}
#END TRANSCRIPT#

Your Turn:
```

**Uruk adaptation:** Use three initial alternatives when the approved candidate/budget limits permit; otherwise record the smaller bound. Persist each contribution and enforce the §6 turn limit in Rust. Replace `HYPOTHESIS` with the typed final/partial result. Once a candidate is stored as an item, refinements create linked children rather than rewriting it. A confident fictional panel is not independent validation.

### 15.4 Reflection through observations

**Source:** Appendix A.2.2, Fig. A.3, p. 42. **Template:** `reflection.observation`.

```text
You are an expert in scientific hypothesis evaluation. Your task is to analyze the
relationship between a provided hypothesis and observations from a scientific article.
Specifically, determine if the hypothesis provides a novel causal explanation
for the observations, or if they contradict it.

Instructions:

1. Observation extraction: list relevant observations from the article.
2. Causal analysis (individual): for each observation:
   a. State if its cause is already established.
   b. Assess if the hypothesis could be a causal factor (hypothesis => observation).
   c. Start with: "would we see this observation if the hypothesis was true:".
   d. Explain if it’s a novel explanation. If not, or if a better explanation exists,
      state: "not a missing piece."
3. Causal analysis (summary): determine if the hypothesis offers a novel explanation
   for a subset of observations. Include reasoning. Start with: "would we see some of
   the observations if the hypothesis was true:".
4. Disproof analysis: determine if any observations contradict the hypothesis.
   Start with: "does some observations disprove the hypothesis:".
5. Conclusion: state: "hypothesis: <already explained, other explanations more likely,
   missing piece, neutral, or disproved>".

Scoring:
    * Already explained: hypothesis consistent, but causes are known. No novel explanation.
    * Other explanations more likely: hypothesis *could* explain, but better explanations exist.
    * Missing piece: hypothesis offers a novel, plausible explanation.
    * Neutral: hypothesis neither explains nor is contradicted.
    * Disproved: observations contradict the hypothesis.

Important: if observations are expected regardless of the hypothesis, and don’t disprove it,
it’s neutral.

Article:
{article}

Hypothesis:
{hypothesis}

Response {provide reasoning. end with: "hypothesis: <already explained, other explanations
more likely, missing piece, neutral, or disproved>".)
```

**Uruk adaptation:** Require a source locator per extracted observation. Check alternative explanations and whether the observation would occur without the hypothesis; `hypothesis => observation` does not establish the converse. Record the explanatory label separately from scientific support. Replace the fixed prose starters/ending with typed fields and a concise rationale. Contradiction must match the claim's actual scope and necessary assumptions; an empty set or weak evidence must not be forced into disproof.

### 15.5 Ranking by pairwise comparison

**Source:** Appendix A.2.3, Fig. A.4, p. 43. **Template:** `ranking.pairwise`.

```text
You are an expert evaluator tasked with comparing two hypotheses.

Evaluate the two provided hypotheses (hypothesis 1 and hypothesis 2) and determine which one
is superior based on the specified {idea_attributes}.
Provide a concise rationale for your selection, concluding with the phrase "better idea: <1 or 2>".

Goal: {goal}

Evaluation criteria:
{preferences}

Considerations:
{notes}
Each hypothesis includes an independent review. These reviews may contain numerical scores.
Disregard these scores in your comparative analysis, as they may not be directly comparable across reviews.

Hypothesis 1:
{hypothesis 1}

Hypothesis 2:
{hypothesis 2}

Review of hypothesis 1:
{review 1}

Review of hypothesis 2:
{review 2}

Reasoning and conclusion (end with "better hypothesis: <1 or 2>"):
```

**Uruk adaptation:** Judge actual evidence and substantive reviews under the shared rubric, not numerical reviewer scores. Do not claim reviews are independent unless their provenance warrants it. Randomize presentation and map positions back to stable item IDs. Replace the two inconsistent ending phrases with the §7 typed outcome, including draw and insufficient basis. No forced winner when the candidates are incomparable or evidence is inadequate.

### 15.6 Ranking through simulated scientific debate

**Source:** Appendix A.2.3, Fig. A.5, p. 44. **Template:** `ranking.debate`.

```text
You are an expert in comparative analysis, simulating a panel of domain experts
engaged in a structured discussion to evaluate two competing hypotheses.
The objective is to rigorously determine which hypothesis is superior based on
a predefined set of attributes and criteria.
The experts possess no pre-existing biases toward either hypothesis and are solely
focused on identifying the optimal choice, given that only one can be implemented.

Goal: {goal}

Criteria for hypothesis superiority:
{preferences}

Hypothesis 1:
{hypothesis 1}

Hypothesis 2:
{hypothesis 2}

Initial review of hypothesis 1:
{review1}

Initial review of hypothesis 2:
{review 2}

Debate procedure:

The discussion will unfold in a series of turns, typically ranging from 3 to 5, with a maximum of 10.

Turn 1: begin with a concise summary of both hypotheses and their respective initial reviews.

Subsequent turns:

    * Pose clarifying questions to address any ambiguities or uncertainties.
    * Critically evaluate each hypothesis in relation to the stated Goal and Criteria.
    This evaluation should consider aspects such as:
        - Potential for correctness/validity.
        - Utility and practical applicability.
        - Sufficiency of detail and specificity.
        - Novelty and originality.
        - Desirability for implementation.
    * Identify and articulate any weaknesses, limitations, or potential flaws in either hypothesis.

Additional notes:
{notes}

Termination and judgment:

Once the discussion has reached a point of sufficient depth (typically 3-5 turns, up to 10 turns)
and all relevant questions and concerns have been thoroughly addressed, provide a conclusive judgment.
This judgment should succinctly state the rationale for the selection.
Then, indicate the superior hypothesis by writing the phrase "better idea: ",
followed by "1" (for hypothesis 1) or "2" (for hypothesis 2).
```

**Uruk adaptation:** Replace the assertion of unbiased experts with an instruction to apply the common rubric; test positional bias rather than assuming it is absent. “Only one can be implemented” applies only if the plan actually has that constraint. Pass the persisted transcript and turn budget, keep both item versions fixed, and return a draw/insufficient-basis result when justified. Improvements suggested during comparison are follow-up Evolution requests, not mutations that change the contestants mid-match.

### 15.7 Evolution for feasibility

**Source:** Appendix A.2.4, Fig. A.6, p. 45. **Template:** `evolution.feasibility`.

```text
You are an expert in scientific research and technological feasibility analysis.
Your task is to refine the provided conceptual idea, enhancing its practical implementability
by leveraging contemporary technological capabilities. Ensure the revised concept retains
its novelty, logical coherence, and specific articulation.

Goal: {goal}

Guidelines:
1. Begin with an introductory overview of the relevant scientific domain.
2. Provide a concise synopsis of recent pertinent research findings and related investigations,
   highlighting successful methodologies and established precedents.
3. Articulate a reasoned argument for how current technological advancements can facilitate
   the realization of the proposed concept.
4. CORE CONTRIBUTION: Develop a detailed, innovative, and technologically viable alternative
   to achieve the objective, emphasizing simplicity and practicality.

Evaluation Criteria:
{preferences}

Original Conceptualization:
{hypothesis}

Response:
```

**Uruk adaptation:** Supply current reviews, actual resource constraints, and retrieved method evidence. State which capability or assumption makes the alternative more feasible, along with remaining limitations. “Contemporary” is not a license to invent available technology. Insert a new child with a discriminating test; do not inherit the parent's support, Elo, or market settlement.

### 15.8 Evolution through analogy and divergence

**Source:** Appendix A.2.4, Fig. A.7, p. 45. **Template:** `evolution.analogy`.

```text
You are an expert researcher tasked with generating a novel, singular hypothesis
inspired by analogous elements from provided concepts.

Goal: {goal}

Instructions:
1. Provide a concise introduction to the relevant scientific domain.
2. Summarize recent findings and pertinent research, highlighting successful approaches.
3. Identify promising avenues for exploration that may yield innovative hypotheses.
4. CORE HYPOTHESIS: Develop a detailed, original, and specific single hypothesis
   for achieving the stated goal, leveraging analogous principles from the provided
   ideas. This should not be a mere aggregation of existing methods or entities. Think out-of-the-box.

Criteria for a robust hypothesis:
{preferences}

Inspiration may be drawn from the following concepts (utilize analogy and inspiration,
not direct replication):
{hypotheses}

Response:
```

**Uruk adaptation:** Name the transferable principle, where the analogy could fail, and a prediction that distinguishes the child from its inspirations. Preserve all parent IDs and distinguish borrowed evidence from evidence for the new mechanism. Produce one coherent candidate, not an inflated list or arbitrary combination.

### 15.9 Meta-review of recurring critiques

**Source:** Appendix A.2.5, Fig. A.8, p. 46. **Template:** `meta.review`.

```text
You are an expert in scientific research and meta-analysis.
Synthesize a comprehensive meta-review of provided reviews
pertaining to the following research goal:

Goal: {goal}

Preferences:
{preferences}

Additional instructions:
{instructions}

Provided reviews for meta-analysis:
{reviews}

Instructions:
    * Generate a structured meta-analysis report of the provided reviews.
    * Focus on identifying recurring critique points and common issues raised by reviewers.
    * The generated meta-analysis should provide actionable insights for researchers
      developing future proposals.
    * Refrain from evaluating individual proposals or reviews;
      focus on producing a synthesized meta-analysis.

Response:
```

**Uruk adaptation:** This is qualitative review synthesis, not a statistical meta-analysis of experiments. Include relevant match critiques and failed-check summaries with provenance. For each recurring issue, cite supporting records, note counterexamples/limited scope, recommend a check, and identify which roles should receive it. Do not create new findings or change rankings here. Keep this strategy separate from Meta-review's research-overview/report strategy, which may summarize individual candidates under §5.

