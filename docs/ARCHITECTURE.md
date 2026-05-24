# ai-memory - Architecture

> One canonical doc for "what is this thing and how is it shaped".
> Long-form research lives next to this file under [`docs/`](.); this
> page is the operational summary for someone reading the code.

## Purpose

ai-memory is a single Rust binary that gives AI coding agents (Claude
Code, OpenAI Codex, OpenCode) long-term memory shared across CLIs.
Quit one mid-task; open another in the same directory; continue. No
manual `write_note` ceremony, no copy-pasting summaries between
sessions.

The artifact you accrete is a **Karpathy-style LLM wiki**: a
git-versioned tree of markdown pages on disk that gets *compiled* over
time, appended-to. Pages are versioned in place via
supersession, semantic concepts compound, episodic logs decay. A
companion SQLite index gives FTS5 + optional vector retrieval; the
markdown stays the source of truth.

## Data flow

```
                ┌──────────────────────┐
                │ Claude Code / Codex  │
                │ / OpenCode CLI       │
                └──────────┬───────────┘
   lifecycle hooks         │  stdio
   (fire-and-forget HTTP)  ▼
                ┌────────────────────────────────────────────┐
                │   ai-memory serve  (one binary)            │
                │ ┌────────────────┐  ┌────────────────────┐ │
                │ │  /hook router  │  │ MCP server (rmcp)  │ │
                │ │ (ai-memory-    │  │ stdio + streamable │ │
                │ │  hooks)        │  │ HTTP /mcp          │ │
                │ └────────┬───────┘  └─────────┬──────────┘ │
                │          │ sanitize           │ tools/call │
                │          ▼                    ▼            │
                │       ┌────────────────────────────┐       │
                │       │  WriterHandle (mpsc)       │       │
                │       │  single dedicated thread,  │       │
                │       │  one rusqlite Connection,  │       │
                │       │  WAL mode, busy_timeout 5s │       │
                │       └────────┬───────────┬───────┘       │
                │                │           │               │
                │  ┌─────────────▼──┐  ┌────▼──────────────┐ │
                │  │ markdown wiki/ │  │ SQLite db/        │ │
                │  │ (source of     │  │ memory.sqlite     │ │
                │  │ truth + git)   │  │ (derived index)   │ │
                │  └────────────────┘  │  - pages + FTS5   │ │
                │           ▲          │  - observations   │ │
                │ watcher   │          │  - handoffs       │ │
                │ ▼─────────┘          │  - page_embeddings│ │
                │   reconciliation     │  - audit_log      │ │
                │   every 30s          └───────────────────┘ │
                └────────────────────────────────────────────┘
                  ▲                                       ▲
                  │     scheduled / on-demand jobs        │
            ┌─────┴───────────┐                  ┌────────┴─────┐
            │ ai-memory       │                  │ ai-memory    │
            │ consolidate     │                  │ forget-sweep │
            │ (LLM-driven)    │                  │ (M8 decay)   │
            └─────────────────┘                  └──────────────┘
            ┌─────────────────┐                  ┌──────────────┐
            │ ai-memory lint  │                  │ ai-memory    │
            │ (rule + LLM)    │                  │ embed (M9    │
            └─────────────────┘                  │ backfill)    │
                                                 └──────────────┘
```

**Steady-state loop:**

1. Agent CLI emits a lifecycle hook (SessionStart, UserPromptSubmit,
   PostToolUse, …) → vendored shell script `curl`s the event JSON to
   `POST /hook` with a sub-second timeout. Agent never blocks.
2. Server's hook router sanitises the payload (the only path from
   untrusted text into the store), assigns an [`ObservationKind`], and
   enqueues a `WriteCmd` to the writer actor. `log.md` gets an
   appended `## [YYYY-MM-DDTHH:MM:SSZ] <event> | <title>` line.
3. On `SessionEnd`, the server synthesises a `sessions/<id>.md`
   summary page (rule-based, no LLM) and opens a `Handoff` row for the
   next agent. Auto-commits the wiki.
4. When `AI_MEMORY_LLM_PROVIDER` is set, `memory_consolidate` rewrites
   that summary into a richer durable page or fans out into a
   multi-page batch under `concepts/`, `decisions/`, `gotchas/`.
5. `memory_query` answers via FTS5; when an embedder is configured,
   via RRF fusion of FTS5 + cosine over `page_embeddings`. Every
   query hit bumps `access_count` + `last_accessed_at` on the page -
   the M8 reinforcement term.
6. The forget sweep runs on demand (or on a future schedule): pages
   with `retention < cold_threshold` are soft-deleted; soft-deletions
   older than `hard_delete_after_days` with no subsequent access get
   purged. Semantic / pinned / freshly-touched pages survive.
7. Backups: `ai-memory backup --to <tarball>` uses SQLite's online
   backup API so the source stays writable; `ai-memory restore`
   reverses. Or: `git push` the wiki dir + `rsync` the data dir.

## Storage architecture

**Two layers, one source of truth.**

* `<data_dir>/wiki/` - markdown source of truth. Owned by a `git2`
  repo so every consolidation pass + every session-end produces a
  durable commit. Editable by hand in Obsidian / vim - the watcher
  reconciles outside edits.
* `<data_dir>/db/memory.sqlite` - derived index. WAL mode. One
  writer actor owns the writer `Connection`; reads go through a
  cloneable read-only pool.
* `<data_dir>/raw/` - reserved for raw session log archives (future).
* `<data_dir>/logs/` - rolling daily `tracing` output.
* `<data_dir>/models/` - reserved for bundled embedding models
  (M9.5+, when local `ort` lands).

**Schema (current head):**

| Table | What |
|---|---|
| `workspaces`, `projects` | Top of the 3-tuple identity coordinate. |
| `pages` | Versioned wiki pages with `is_latest` + `supersedes` chain. M8 columns: `last_accessed_at`, `access_count`, `superseded_at`. M9 cols: `embedding_provider`, `embedding_model`, `embedding_dim`. |
| `pages_fts` | FTS5 virtual table over `(title, body)`, auto-synced by triggers. |
| `sessions`, `observations` | Hook capture, full audit log. |
| `links` | Wiki cross-references with `to_page_id` nullable for unresolved. |
| `handoffs` | Typed cross-agent handoff records (open / accepted / expired). |
| `page_embeddings` | One vector per latest page, with `(provider, model, dim)` denormalised for refuse-on-mismatch checks. |
| `audit_log` | Every mutation, addressable by `at DESC`. |

**Memory tiers (M8 policy):**

| Tier | Lifetime | Decay |
|---|---|---|
| Working | Current session only | Hard-drop on session end (kept in `observations` for forensics) |
| Episodic | 30d hot → 180d cold → evict | `salience · exp(−λΔt) + σ · log(1+access_count) · exp(−μ · days_since_access)` |
| Semantic | Indefinite | None - only supersedeable via M7 LLM rewrite |
| Procedural | Indefinite | Frequency-decay if not re-observed |

Pinned pages (`pinned: true` in frontmatter) are exempt from all
decay paths.

## Crate layout

```
crates/
├── ai-memory-core/        domain types, errors, ids. NO IO.
├── ai-memory-store/       SQLite + writer actor + reader pool + decay math.
├── ai-memory-wiki/        atomic markdown writes, file watcher, git.
├── ai-memory-mcp/         rmcp transport + tool router.
├── ai-memory-hooks/       payload schemas, sanitiser, /hook ingress.
├── ai-memory-llm/         LlmProvider + Embedder traits + 3 providers + 2 embedders.
├── ai-memory-consolidate/ Karpathy ingest / lint / sweep pipeline.
└── ai-memory-cli/         `ai-memory` binary entry point + 17 subcommands.
```

Each crate has a single responsibility and exposes a typed API. No
circular deps. Inter-crate boundaries enforce the cross-cutting
invariants below.

## MCP tool surface (10 tools)

| Tool | Hint | Purpose |
|---|---|---|
| `memory_query` | read-only | FTS5 or hybrid RRF search. Bumps access counters. |
| `memory_recent` | read-only | Most-recently-updated `is_latest=1` pages. |
| `memory_status` | read-only | Counts, paths, version. |
| `memory_handoff_begin` | destructive | Open a handoff for the next agent. |
| `memory_handoff_accept` | destructive | Fetch + ack the latest open handoff (auto-cwd-matched). |
| `memory_consolidate` | destructive | LLM-driven page rewrite. `multi_page=true` for atomic fan-out. |
| `memory_forget_sweep` | destructive | M8 retention pass. `dry_run=true` for preview. |
| `memory_lint` | destructive | Rule-based + LLM contradiction findings → `wiki/_lint/`. |

All MCP tools carry `readOnlyHint` / `destructiveHint` annotations so
the calling agent can pick safely. Parameter aliases (`query|q|search`,
`workspace|ws`) absorb LLM verbal variance.

## CLI subcommand surface (17 commands)

```
init               watch              embed
status             serve              forget-sweep
search             reset              lint
write-page         backup             commit
                   restore            llm-test
                   install-hooks
```

Run `ai-memory --help` for the full tree.

## Cross-cutting invariants

Carved in M0/M1; every milestone has to respect them. Each comes from
a documented prior-art bug; cite the source when reviewing changes
that touch the relevant area.

1. **One config-read path.** `Config::load()` called once at startup.
   No `std::env::var` outside it.  (agentmemory #456 / #469.)
2. **Single-writer SQLite actor.** All writes go through one `mpsc`
   channel to one dedicated OS thread. (cognee #2717.)
3. **Indexes commit in the same transaction as the data.** No
   background-task-indexing-after-return. (basic-memory #763 / #578.)
4. **Typed 3-tuple identity** (`workspace_id`, `project_id`, path)
   in every domain row from day one. (basic-memory #783 / #834.)
5. **Hooks are fire-and-forget.** Hook scripts hard-timeout at
   ≤200 ms; server returns 202 immediately. (agentmemory #221 / #143.)
6. **Privacy strip is a typed boundary.** `Sanitized<NewObservation>`
   has no other constructor than `sanitize()`. (design-decisions §14.)
7. **JSON-schema structured outputs only.** Native provider JSON
   modes; no XML, no Instructor wrapping. (agentmemory #492 / #539,
   cognee #2840.)
8. **`{provider, model, dim}` denormalised next to every embedding.**
   Refuse-on-mismatch at startup. (agentmemory #469.)
9. **Live-process check before destructive ops.** `ai-memory reset`,
   `backup`, `restore` all consult `sysinfo`. (basic-memory #765.)
10. **Atomic file writes** (tmp + rename + fsync). Watcher ignores
    own writes by filename prefix.
11. **Absolute canonical data dir** default; logged loudly on
    startup. (agentmemory #303.)
12. **No global singletons / `lazy_static` configs.** All deps
    explicit. (cognee #2228.)
13. **Zero-LLM default path.** LLM has opt-in via env. The
    system works without any provider configured.
14. **Tracing subscribers explicitly filter their own module.**
    No feedback loops. (agentmemory #519.)

## Configuration (`config.toml`)

Lives at `<data_dir>/config.toml`. All values overridable by env vars
prefixed `AI_MEMORY_*`.

```toml
bind = "127.0.0.1:49374"
log_level = "info"

[decay]                            # M8 retention params
lambda = 0.02                      # ↓ to forget less aggressively
sigma = 0.6                        # ↑ to reward query-hits more
mu = 0.04                          # ↑ if recent hits should count more
cold_threshold = 0.20              # below this → soft-delete
hard_delete_after_days = 180
```

**LLM provider env** (opt-in):
```
AI_MEMORY_LLM_PROVIDER     anthropic | openai | openai-compat
AI_MEMORY_LLM_MODEL        e.g. claude-sonnet-4-6
ANTHROPIC_API_KEY / OPENAI_API_KEY / LLM_API_KEY
AI_MEMORY_LLM_BASE_URL     for openai-compat (Ollama, vLLM)
```

**Embedder env** (opt-in):
```
AI_MEMORY_EMBEDDING_PROVIDER   openai | voyage
AI_MEMORY_EMBEDDING_MODEL      e.g. text-embedding-3-small
AI_MEMORY_EMBEDDING_DIM        1536
OPENAI_API_KEY / VOYAGE_API_KEY
```

## Future work

* **M9.5 - local embeddings via `ort`.** Bundle `bge-small-en-v1.5`
  for an API-key-free homelab path. ~200 MB image bloat; trait is
  ready, just needs the `OrtBgeSmallEmbedder` impl + tokenizer wiring.
* **`sqlite-vec` integration.** Brute-force cosine works fine to a few
  thousand pages; past that, the `sqlite-vec` extension is the next
  step. Deferred behind workaround for rusqlite-compat (issue #206).
* **Scheduled forget-sweep / lint.** Currently manual / on-demand via
  the MCP tools or CLI. The next iteration runs both on a 6-hour
  `tokio::interval` inside `serve`.
* **Multi-workspace UI / web dashboard.** Out of scope for v1; revisit
  once the headless server has been load-tested.
* **Real LongMemEval-S harness.** The recall-eval framework exists
  ([`crates/ai-memory-consolidate/tests/recall_eval.rs`](../crates/ai-memory-consolidate/tests/recall_eval.rs));
  porting LongMemEval-S itself requires the dataset.

## Reading order

* This file - operational summary, you are here.
* [`docs/design-decisions.md`](design-decisions.md) - the full v1 spec.
* [`docs/research-karpathy-llm-wiki.md`](research-karpathy-llm-wiki.md)
 - what "Karpathy-faithful" means.
* [`docs/research-agentmemory.md`](research-agentmemory.md),
  [`research-basic-memory.md`](research-basic-memory.md),
  [`research-cognee.md`](research-cognee.md) - prior art studied.
* [`docs/issues-*.md`](.) - concrete failure modes we've designed to
  avoid.
* [`CLAUDE.md`](../CLAUDE.md) - per-session operating rules pinned
  into Claude Code conversations.
