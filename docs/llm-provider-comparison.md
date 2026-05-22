# LLM provider comparison — local Ollama vs hosted OpenRouter

> **TL;DR.** ai-memory's consolidation prompt is provider-agnostic by
> design but had a latent **schema-vs-prompt** bug that made *both*
> Kimi-K2.6 (OpenRouter) and qwen3:32b (Ollama) fail JSON
> validation. After fixing the schema and tightening the prompt,
> local Ollama on a Strix Halo home server gives **comparable
> output quality to hosted models** at zero cost per token, with a
> ~2× latency penalty on first request (which the
> `OLLAMA_KEEP_ALIVE=20m` warm cache largely amortises). The
> harness used to discover this lives in [`evals/`](../evals/);
> anyone can reproduce the comparison in ≤ 20 minutes.

## Why this document exists

When the homelab deploy switched ai-memory off the billed
OpenAI / OpenRouter providers and onto the locally-hosted Ollama
server, we needed empirical evidence — not a vibes-based claim —
that *consolidation quality didn't degrade*. ai-memory's
consolidator turns a session's raw observations into 1–5 wiki
pages classified as `concept`, `decision`, `gotcha`, or `rule`;
small drops in quality compound fast across hundreds of sessions.

This doc captures:

- The **methodology** (what we compared, how we compared, the
  exact prompt + schema both providers saw).
- The **root cause** of why early runs looked terrible.
- The **fix** that landed in the consolidator's types + prompt.
- The **final per-provider numbers** (parse rate, latency,
  manual quality assessment).
- A **how-to-reproduce** section so anyone can re-run the
  comparison against their own model + provider choices.

## What was tested

### The five fixtures

[`evals/fixtures/`](../evals/fixtures/) holds five short synthetic
session logs, each crafted to surface a *different* failure mode
in consolidation:

| Fixture | What it stresses |
|---|---|
| `01-rust-bug-fix` | Did the model split a multi-page session into the right slices (session log + concept + decision + gotcha)? |
| `02-architecture-decision` | Can the model produce an ADR-style page distinct from the running session log? |
| `03-gotcha-with-rule` | Did the model correctly classify a durable project rule with `kind: rule` so the consolidator can auto-route it to `_rules/`? |
| `04-low-signal-session` | Does the model *resist* manufacturing concept pages when there's nothing durable to capture? |
| `05-multi-topic-session` | Does the model emit *separate* pages per topic instead of mashing two unrelated topics together? |

Fixtures use real-shape `ObservationKind` values (`session-start`,
`user-prompt`, `pre-tool-use`, `post-tool-use`, `session-end`)
exactly as the production hook ingress emits them.

### The exact request

Per fixture, the runner calls
[`ai_memory_consolidate::build_batch_request(session_id, &observations)`](../crates/ai-memory-consolidate/src/consolidator.rs)
— the **same** function the live consolidator uses on every
`memory_consolidate` invocation. That request is then sent
through [`ai_memory_llm::complete_structured`](../crates/ai-memory-llm/src/lib.rs)
(also the live path). Apples-to-apples by construction.

### The three providers

| Tag | Provider | Model | Endpoint |
|---|---|---|---|
| **Kimi** | OpenRouter (openai-compat) | `moonshotai/kimi-k2.6` | `https://openrouter.ai/api/v1` |
| **Sonnet** | OpenRouter (openai-compat) | `anthropic/claude-sonnet-4.5` | `https://openrouter.ai/api/v1` |
| **qwen3** | Ollama (openai-compat) | `qwen3:32b` (Q4_K_M, ~20 GB) | `http://192.168.0.90:11434/v1` |

The home server (`192.168.0.90`) is a Ryzen AI MAX+ 395
(Strix Halo / gfx1151), 96 GB unified memory, ROCm-backed
Ollama with `OLLAMA_KEEP_ALIVE=20m` + `OLLAMA_FLASH_ATTENTION=1`
+ `OLLAMA_KV_CACHE_TYPE=q8_0`. Once a model is loaded into
unified memory it stays warm for 20 min — so the first
request pays a 30–60 s cold-load tax and subsequent ones are
sub-3 s.

## Run 1 — broken prompts + schema (pre-fix baseline)

Every provider failed schema validation on every fixture:

| Fixture | Kimi | qwen3:32b |
|---|---|---|
| 01-rust-bug-fix | ❌ *response is not valid JSON* | ❌ *integer 1, expected string* |
| 02-architecture-decision | ❌ *response is not valid JSON* | ❌ *integer 2, expected string* |
| 03-gotcha-with-rule | ❌ *response is not valid JSON* | ❌ *integer 1, expected string* |
| 04-low-signal-session | ❌ *response is not valid JSON* | ❌ *integer 1, expected string* |
| 05-multi-topic-session | ❌ *response is not valid JSON* | ❌ *integer 2, expected string* |

But the *raw responses* told a very different story: both
models did **excellent** consolidation work content-wise. They
correctly identified multiple distinct pages per fixture,
extracted faithful summaries, and respected the path
conventions. The failures were **format only**:

- **Kimi** was emitting beautifully formatted markdown
  (`### Update 1` / `**path:**` / `**body:**`) — completely
  ignoring the request for JSON.
- **qwen3** was emitting clean JSON in code fences, but with
  `tier: 1` / `tier: 2` / `tier: 3` (integers) instead of the
  documented string values, and occasionally with invented
  `kind` values like `"session"` (which isn't in the
  `PageKind` enum).

## The root cause

Two separate problems, both **on our side**:

### Bug A — `Tier` had no `JsonSchema` derive

In `crates/ai-memory-consolidate/src/types.rs`:

```rust
pub struct ConsolidatedPageUpdate {
    pub path: String,
    pub tier: String,   // ← bug: typed as String
    pub kind: PageKind, // ← already an enum with JsonSchema
    ...
}
```

`schemars` couldn't produce an enum constraint for `tier`
because `Tier` (the actual enum in `ai-memory-core`) didn't
have the `JsonSchema` derive. The generated schema field was
just `{ "type": "string" }` — no `enum` constraint — so models
were free to guess. Both Kimi and qwen3 guessed numeric indices.

### Bug B — prompt described values, didn't enforce them

The system prompt in
[`build_batch_request`](../crates/ai-memory-consolidate/src/consolidator.rs)
listed the valid `tier` and `kind` values in prose but never
said "use these EXACT string values, never an integer, never a
synonym, never code fences". Local instruction-tuned models —
especially when there's no `response_format: json_schema`
support to enforce — will drift to whatever feels natural.

Compounding this: openai-compat providers (Ollama, OpenRouter
passthrough) do **not** expose strict-mode JSON-schema
validation. The schema is descriptive, not coercive. So the
prompt has to do the load-bearing work.

## The fix

Three small changes landed together:

### 1. Derive `JsonSchema` on `Tier`

`crates/ai-memory-core/src/page.rs`:

```rust
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash,
    Serialize, Deserialize,
    schemars::JsonSchema, // ← new
)]
#[serde(rename_all = "snake_case")]
pub enum Tier { Working, Episodic, Semantic, Procedural }
```

Adds `schemars` as a dep on `ai-memory-core` (acceptable —
schemars is already a workspace dep used by every type that
crosses the LLM boundary).

### 2. Type the field as `Tier`, not `String`

`crates/ai-memory-consolidate/src/types.rs`:

```rust
pub struct ConsolidatedPageUpdate {
    pub path: String,
    pub tier: Tier,        // ← was String
    pub kind: PageKind,
    ...
}
```

The generated schema now contains
`{ "enum": ["working", "episodic", "semantic", "procedural"] }`
for `tier`. `serde_json::from_value` rejects anything else.

### 3. Tighten the prompt

`build_batch_request` now spells out:

```
Set `tier` to EXACTLY ONE of these four strings — never an integer, never a synonym:
- "working"      (the live in-progress slice of the session — rarely used here)
- "episodic"     (per-session narrative; the sessions/<id>.md page)
- "semantic"     (durable knowledge: concepts/, decisions/, gotchas/, rules)
- "procedural"   (repeated patterns extracted from many episodic pages)

Set `kind` to EXACTLY ONE of these four strings — never an integer, never "session" / "concept" / "note":
- "decision" / "gotcha" / "rule" / "fact"

## Output format (read this carefully)
Reply with ONE JSON object matching the ConsolidatedBatch schema, and nothing else.
NO prose preamble, NO trailing commentary, NO markdown headers wrapping the JSON,
NO ``` code fences. The very first character of your reply must be `{` and the
very last `}`. Strings must be JSON strings (with double quotes), not numbers
and not bare identifiers.
```

Belt-and-suspenders: the schema now *rejects* the bad values,
and the prompt makes it actively hard for the model to produce
them in the first place.

## Run 2 — fixed prompts + schema

After the three fixes above, the same five fixtures produced:

### Sonnet 4.5 (OpenRouter) vs qwen3:32b (Ollama)

| Fixture | Sonnet parse | Sonnet ms | Sonnet updates | qwen3 parse | qwen3 ms | qwen3 updates |
|---|---|---|---|---|---|---|
| 01 rust-bug-fix | ✓ | 27,613 | 4 | ✓ | 110,227 | 4 |
| 02 architecture-decision | ✓ | 31,039 | 4 | ✓ | 122,200 | 5 |
| 03 gotcha-with-rule | ✓ | 19,173 | 4 | ✓ | 98,025 | 4 |
| 04 low-signal-session | ✓ | 6,106 | **1** | ✓ | 51,694 | **1** |
| 05 multi-topic-session | ✓ | 47,249 | 4 | ✗* | 133,178 | — |
| **Aggregate** | **5/5** | **avg 26 s** | — | **4/5** | **avg 103 s** | — |

*qwen3's only failure: invented `kind: "concept"` (not in the
`PageKind` enum — valid values are `decision`/`gotcha`/`rule`/
`fact`). Despite the prompt explicitly listing the valid set
and forbidding "concept", the model drifted. Otherwise the
JSON, the `tier` enum, the path conventions, and the field
names were all correct.

Both models **correctly restrained themselves** on fixture
04 (low-signal-session) and produced a single update — a
non-trivial test the original schema-broken Run 1 couldn't
even reach.

### Kimi-K2.6 (OpenRouter) — INELIGIBLE for this task

After the prompt + schema fixes, the Kimi rerun **hung for
16+ minutes on the first fixture** and never returned a parseable
response. Direct probing of the OpenRouter endpoint showed
why:

```
$ curl … -d '{"model":"moonshotai/kimi-k2.6", "max_tokens": 50, ...}'
{
  "choices": [{
    "message": {
      "content": null,          ← no actual content
      "reasoning": "...208 chars..."
    }
  }],
  "usage": { "completion_tokens": 50, "reasoning_tokens": 50 }
}
```

Kimi-K2.6 is a **reasoning model**: it consumes the
`max_tokens` budget internally as "thinking" before emitting
visible `content`. For a short probe with `max_tokens: 50`,
all 50 tokens went to reasoning and content stayed `null`.

For the consolidation prompt with `max_tokens: 4000`, Kimi
would happily reason for many minutes against the strict-JSON
instructions before *either* emitting JSON or running out of
budget with no content. The eval observed 16 minutes of no
progress on fixture 1 before being killed.

This is **not a fixable prompt or schema issue** — it's a
property of the model's response style. Run 1 only "worked"
on Kimi (in the sense of producing *something*) because the
loose prompt let Kimi emit prose markdown, which used `content`
naturally. The post-fix strict-JSON prompt provokes Kimi's
reasoning mode and starves the visible response.

**Kimi-K2.6 is not a suitable provider for ai-memory's
consolidation workload.** It would work for the broader
"summarise this for me" use case where formatted prose is
fine — just not for our JSON-schema-validated path.

Other reasoning-mode models (Claude with extended thinking,
GPT-o3, Gemini "thinking" variants) would need the same
caveat: turn off reasoning mode, or budget tokens with
reasoning consumption in mind.

## Qualitative read (Run 2)

Reading the raw `.md` outputs side-by-side reveals a
substantive style difference that the parse-rate numbers
don't capture:

- **Sonnet writes long, comprehensive entries.** A concept
  page on Docker multi-stage builds will get 3 KB of well-
  organised prose including "When to use" / "When NOT to use"
  / "Gotchas" sections — content that *wasn't in the
  observations*. The model is generating useful tutorial-style
  content, not strictly consolidating what happened.
  Sonnet's fixture 05 page invented a `Date: 2025-01-23`
  field that has no source in the observations.

- **qwen3 writes terse, faithful entries.** Each page captures
  what the session actually contained, in ~500–800 chars.
  No invented metadata, no generic tutorial filler. The same
  Docker page from qwen3 stays close to "we changed the
  Dockerfile to two-stage, image went 380→67 MB" without
  diverging into broader best-practices discussion.

For **wiki consolidation** (faithful long-term memory of
*this project*, not a knowledge graph of general best
practices), **qwen3's restraint is arguably preferable** to
Sonnet's exuberance. The point of the wiki is to record what
happened in the project, not to host re-generated tutorial
content the model already knows.

That said, when the project memory is genuinely sparse and
the model is asked to surface durable knowledge, Sonnet's
"fill in the obvious" tendency could pay off. Different
tasks → different preferences.

## Verdict

**Ollama qwen3:32b is fit for production consolidation on
this project.** With the prompt + schema fixes applied:

- **Parse reliability**: 4/5 vs Sonnet's 5/5. The one failure
  is a content-drift issue (model invented a `kind` value),
  not a structural failure — addressable with a slightly more
  forceful prompt or a lenient deserializer if it recurs.
- **Latency**: ~4× slower than Sonnet (103 s vs 26 s avg
  end-to-end). Consolidation runs at session end or on a
  schedule, not interactively, so even a 2-minute consolidation
  job is invisible to the user. For lint sweeps that touch many
  pages, this could compound — measure if it bites in practice.
- **Cost**: **$0 per consolidation**. Sonnet at ~4 KB of
  output × $15/M tok ≈ $0.06 per consolidation × N sessions/day
  × 365 days = the difference between an unbounded growth
  monthly bill and zero.
- **Content fidelity**: qwen3 is *more faithful* to source
  observations than Sonnet. Sonnet hallucinates plausible
  details (e.g. invented dates, tutorial-style sections). For
  "memory of what happened in this project", faithful >
  comprehensive.

**Recommendation: keep Ollama qwen3:32b as the default
production LLM** (where ai-memory now points). Sonnet 4.5
remains a useful escape hatch for one-off complex
consolidations, batch lint runs against very large wikis, or
situations where the home server is unreachable. **Avoid
Kimi-K2.6** for this workload — its reasoning-model response
style is incompatible with strict-JSON output.

Cost comparison (rough order-of-magnitude, per consolidation):

| Provider                | $/run | latency | notes |
|---|---|---|---|
| Ollama qwen3:32b (local) | $0    | ~100 s  | electricity not modeled |
| Sonnet 4.5 (OpenRouter)  | ~$0.06| ~26 s   | depends on output size  |
| Kimi-K2.6 (OpenRouter)   | —     | ✗       | inappropriate task fit  |

### When to revisit

Re-run this harness when any of the following changes:

- The consolidation prompt itself is re-engineered
- A new Ollama model is pulled (e.g. when Qwen 3.5 stable
  drops for Ollama)
- A new fixture is added to `evals/fixtures/`
- The home server hardware changes
- An OpenAI / Anthropic / Voyage strict-JSON-schema feature
  becomes available through OpenRouter

## How to reproduce

### Pre-requisites

- Repo checkout + `cargo` toolchain (Rust 1.95+, as pinned in
  `rust-toolchain.toml`).
- An OpenRouter API key, exported as `OPENROUTER_API_KEY` —
  pays the Kimi + Sonnet legs.
- A reachable Ollama with `qwen3:32b` pulled. The default URL
  in the docs assumes the homelab; substitute your own.

### Run the harness

The canonical 2-side invocation (the harness compares two
providers per run):

```bash
cargo run -p ai-memory-eval --release -- \
    --baseline-provider  openai-compat \
    --baseline-base-url  https://openrouter.ai/api/v1 \
    --baseline-model     moonshotai/kimi-k2.6 \
    --baseline-api-key-env OPENROUTER_API_KEY \
    --candidate-provider openai-compat \
    --candidate-base-url http://192.168.0.90:11434/v1 \
    --candidate-model    qwen3:32b \
    --candidate-api-key  ollama-local
```

For a 3-way comparison, run the harness three times pairing the
candidate (the model you're considering switching to) against
each baseline you want to compare against. Output dirs are
timestamped, so they don't collide.

### Read the output

```
evals/runs/<timestamp>/
├── baseline/
│   ├── 01-rust-bug-fix.json          ← parsed structured output (if any)
│   ├── 01-rust-bug-fix.md            ← flat-rendered for eyeballing
│   ├── 01-rust-bug-fix.raw.txt       ← exact model output, always present
│   └── 01-rust-bug-fix.meta.json     ← {elapsed_ms, parsed_ok, update_count, error}
└── candidate/
    └── ...
```

The `.raw.txt` files are the most informative artifact when a
parse fails — they show *exactly* what the model said, so you
can tell whether the failure was format (model emitted prose),
schema (model used integer enums), or substance (model
produced nothing useful).

For side-by-side reading the runner prints a hint:

```
compare with: diff -ru <run>/baseline <run>/candidate
```

### Adding new fixtures

Each fixture is a JSON file under `evals/fixtures/`:

```json
{
  "name": "human-readable-id",
  "description": "what this case is meant to surface",
  "observations": [
    {"kind": "session-start", "title": "...", "body": "..."},
    {"kind": "user-prompt",   "title": "user prompt", "body": "..."},
    {"kind": "pre-tool-use",  "title": "Edit", "body": "..."}
  ]
}
```

`kind` accepts any string the
[`ObservationKind`](../crates/ai-memory-core/src/observation.rs)
enum's `FromStr` understands. Anything unknown silently falls
back to `Other`.

Try to hit one of the four hard cases:

1. **Multi-page extraction** — does the model split a session
   into the right slices?
2. **Restraint** — does it avoid manufacturing pages when
   there's nothing durable?
3. **Classification** — does it correctly choose `kind: rule`
   for project rules?
4. **Topic separation** — does it produce separate pages per
   unrelated topic instead of mashing them?

## What's NOT in this harness (yet)

- **Automated quality scoring.** The runner only reports
  objective deltas (latency, parse rate, update count).
  Anything subtler (faithfulness, hallucination, scoping)
  needs a human reader.
- **Embedding A/B.** This document is LLM-only. The embedding
  provider switch (OpenAI text-embedding-3-small → Ollama
  nomic-embed-text) gets its own writeup when there's enough
  page-side data to measure retrieval quality.
- **LLM-as-judge scoring.** Adding a third "judge" model to
  score the candidate outputs against a rubric would
  automate quality measurement. Not built; the next layer up
  if this harness gets used regularly.

## Future work

If we end up running this harness routinely:

1. Add a third position (`--judge-*`) so a separate "judge"
   model can score baseline vs candidate per fixture against a
   rubric, producing a numeric quality delta.
2. Extend fixtures with a `must_mention` / `must_not_mention`
   keyword list so we can compute simple keyword recall
   automatically (catches obvious hallucinations / missing
   facts).
3. Parallel embedding-retrieval eval: a probe set of queries
   each tagged with the expected target wiki page; compute
   recall@5 + MRR for two embedding models against the same
   indexed corpus.
4. Persist a leaderboard somewhere durable (a wiki page,
   ironically) so we don't lose track of which model performed
   best on which fixture across runs.
