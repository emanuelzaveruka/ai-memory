//! Canonical CLAUDE.md / AGENTS.md routing snippet.
//!
//! The agent only proactively calls `memory_query` / `memory_recent` / etc.
//! when the project's CLAUDE.md tells it WHEN to. This module owns the
//! markdown block that defines that "intent → tool" routing table.
//!
//! Two callers consume it:
//!
//! - `ai-memory-cli`'s `install-instructions` subcommand — writes the
//!   block into `./CLAUDE.md` directly from the host.
//! - `ai-memory-mcp`'s `memory_install_self_routing` MCP tool — returns
//!   the block to the agent, which then uses its own Write/Edit tool
//!   to land it in the target file (the MCP server can't reach the
//!   agent's host filesystem).
//!
//! Keeping the snippet in one constant means "what gets written" stays
//! consistent across both paths; updating it once propagates.

/// HTML-comment marker that opens the managed section. Anything that
/// edits a CLAUDE.md must key off this exact string — install /
/// uninstall / refresh all locate the block by these markers.
pub const MARKER_START: &str = "<!-- ai-memory:start -->";

/// HTML-comment marker that closes the managed section.
pub const MARKER_END: &str = "<!-- ai-memory:end -->";

/// The canonical snippet body. Trimmed of leading/trailing whitespace
/// by callers; wrap with `MARKER_START` + `MARKER_END` before writing.
pub const SNIPPET_BODY: &str = r#"
## Long-term memory (ai-memory)

This project uses [ai-memory](https://github.com/akitaonrails/ai-memory)
for cross-session continuity. **Lifecycle hooks already capture every
prompt + tool call automatically.** You never need to manually write
notes; the SessionStart hook auto-fetches pending handoffs and the
SessionEnd hook auto-consolidates. Just *use* the read tools.

### When to reach for each tool

The user can express any of the intents below in plain English —
match the intent to the tool. They do not need to name the tool.

| User says / situation | Tool |
|---|---|
| "have we discussed X?" / "search memory for Y" / before proposing architecture | `memory_query` |
| "what's been going on" / "show recent activity" (light) | `memory_recent` |
| "is ai-memory healthy?" / "how big is the wiki?" | `memory_status` |
| "give me the stats" / structured snapshot for the agent to consume | `memory_briefing` |
| "catch me up" / "I've been away" / "what's important right now?" / open-ended exploration | `memory_explore` |
| "where did we leave off?" — and you see a `📥 ai-memory: pending handoff` block in your context | already done — answer from that block; do NOT re-call `memory_handoff_accept` |
| "where did we leave off?" — and no such block is visible | `memory_handoff_accept` (rare; the SessionStart hook usually got there first) |
| "save context for the next session" / wrapping up | `memory_handoff_begin` (terse summary; put detail in `open_questions` + `next_steps` bullets) |
| "consolidate this session" / "compile what we learned" (usually automatic) | `memory_consolidate` |
| "audit the wiki" / "find contradictions" / "what rules should we add?" | `memory_lint` |
| "prune old pages" / "memory cleanup" | `memory_forget_sweep` |

`memory_explore` is the right default for the "I want to know what's
going on" use case — it returns a prose digest whose verbosity
scales automatically to how long it's been since the last activity
(< 1 h → one line; > 30 days → full catchup).

### When you write a project rule, write it here

If you're about to write a durable project rule ("always X", "never
Y", "all PRs must …"), this rules file (CLAUDE.md for Claude Code;
AGENTS.md for Codex / OpenCode / Cursor / Gemini CLI; whichever
convention your agent uses) is where it belongs. ai-memory's lint
pass surfaces the same hint automatically when a `kind: rule` page
lands in `_rules/`.

### Refreshing this snippet

This block is maintained by ai-memory. Two ways to refresh it with
the latest binary's recommended copy:

- **From the agent** (no terminal needed): ask "refresh the ai-memory
  routing in this project" — the agent calls
  `memory_install_self_routing`, picks the right filename for itself
  (Claude Code → `CLAUDE.md`; Codex / OpenCode / Cursor / Gemini →
  `AGENTS.md`), and uses its Write / Edit tool to land the block.
- **From the CLI**: `ai-memory install-instructions` (defaults to
  `CLAUDE.md`; pass `--target AGENTS.md` for non-Claude agents).

Both are idempotent: re-runs replace the block bracketed by
`<!-- ai-memory:start -->` / `<!-- ai-memory:end -->` markers
without disturbing the rest of the file.
"#;

/// Build the full markered block that should land in CLAUDE.md /
/// AGENTS.md, including the `<!-- ai-memory:start -->` / `<!-- ai-
/// memory:end -->` wrappers and a trailing newline.
///
/// Both the CLI's `install-instructions` and the MCP tool
/// `memory_install_self_routing` emit this exact string.
#[must_use]
pub fn full_block() -> String {
    format!("{MARKER_START}\n{}\n{MARKER_END}\n", SNIPPET_BODY.trim())
}
