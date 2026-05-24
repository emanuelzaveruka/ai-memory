# MCP install guide - additional clients

> All snippets below default to `http://127.0.0.1:49374` (local server). For a
> remote server (homelab, LAN box) substitute the appropriate URL AND add an
> `Authorization: Bearer <token>` header to the `headers` block when bearer auth
> is enabled. The MCP wire protocol expects the `/mcp` path suffix on the URL.

> **Transport is stateless by default.** Since v0.1.2 the HTTP transport
> answers each request independently (plain JSON, no `Mcp-Session-Id`
> required), so any client that points a remote URL at `/mcp` — including
> OpenCode `type: "remote"` and plain `curl` — works without an
> `mcp-remote` stdio shim (issue #3). The `mcp-remote` bridge is still
> needed for **Claude Desktop** specifically, because its config only
> supports stdio servers — not because of session state. If you run a
> client that *requires* MCP session continuity or server-initiated SSE
> streams, start the server with `ai-memory serve --transport http
> --http-stateful` to restore rmcp's session mode.

This page documents how to register ai-memory as an MCP server with
the agent CLIs **that are not covered inline in the README**.

The three flagship clients - Claude Code, OpenAI Codex, OpenCode -
have lifecycle-hook scripts under [`hooks/`](../hooks) and are
covered in the [main README](../README.md#configure-your-agent-cli).

The clients on this page are **MCP-only**: they expose long-term
memory to their LLM via ai-memory's MCP tools (`memory_query`,
`memory_recent`, `memory_handoff_accept`, etc.), but they cannot
auto-capture session events into ai-memory's `/hook` endpoint
without a per-client plugin written upstream. The trade-off:

| | What you get | What you don't get |
|---|---|---|
| **MCP only** | LLM can query the wiki, accept handoffs, run memory_consolidate | No automatic session-end summaries; no auto-handoff at session boundaries |
| **MCP + hooks** | All of the above *plus* every prompt/tool-call captured automatically; handoffs surface at SessionStart with no human prompting | - |

Most of these clients will land hook integration upstream eventually
(Gemini CLI is the closest); for now, you can still cover the
session-boundary gap by asking the LLM to call `memory_handoff_begin`
manually before quitting.

> **One-shot tip:** every snippet below is also reachable from the
> CLI:
> ```bash
> ai-memory install-mcp --client cursor       # or claude-desktop / gemini-cli / openclaw / pi
> ```

---

## Cursor

**Status:** ✅ MCP supported. ❌ No lifecycle hooks.

**Config file:**
- Per-project: `.cursor/mcp.json` in the workspace root.
- Global: `~/.cursor/mcp.json`.

```json
{
  "mcpServers": {
    "ai-memory": {
      "url": "http://127.0.0.1:49374/mcp"
    }
  }
}
```

**Gotchas:**
- Cursor uses the `url` key for HTTP/SSE transports. Stdio uses
  `command` + `args` instead.
- After editing `mcp.json`, restart Cursor or toggle the server
  off+on in **Settings → MCP**. Live reload is in newer builds but
  still occasionally requires a manual restart.
- Source: <https://cursor.com/docs/context/mcp>

---

## Claude Desktop

**Status:** ✅ MCP supported (via stdio shim for HTTP). ❌ No lifecycle hooks.

**Config file:**
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`
- Linux: not officially distributed by Anthropic. Use Claude Code
  (terminal) instead.

**Important:** Claude Desktop's JSON config supports stdio MCP
servers only. To talk to ai-memory's HTTP endpoint, bridge through
the community [`mcp-remote`](https://www.npmjs.com/package/mcp-remote)
stdio shim. Requires Node.js installed on the same machine.

```json
{
  "mcpServers": {
    "ai-memory": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "http://127.0.0.1:49374/mcp"]
    }
  }
}
```

**Gotchas:**
- After editing the config, **fully quit and relaunch** Claude
  Desktop. "Check for Updates…" is not enough.
- If the MCP indicator doesn't appear after restart, check the logs:
  `~/Library/Logs/Claude/mcp*.log` (macOS) or `%APPDATA%\Claude\logs\`
  (Windows).
- Source: <https://modelcontextprotocol.io/quickstart/user>

---

## Gemini CLI

**Status:** ✅ MCP supported. ⚠️ Hooks exist in the source but are
not yet stable enough to recommend (the user-facing docs are sparse
and event names have shifted between versions). Treat this as
MCP-only for now.

**Config file:**
- User: `~/.gemini/settings.json`
- Project: `.gemini/settings.json`

Gemini CLI uses `httpUrl` (not `url`) for streamable-HTTP MCP
endpoints. The `timeout` is in milliseconds.

```json
{
  "mcpServers": {
    "ai-memory": {
      "httpUrl": "http://127.0.0.1:49374/mcp",
      "timeout": 5000
    }
  }
}
```

**Gotchas:**
- Gemini supports stdio too via `command`/`args`, plus SSE via `url`.
  Only `httpUrl` covers streamable HTTP. Don't mix them in one entry.
- If hooks are surfaced and stabilised upstream, ai-memory will ship
  a Gemini hook bundle in `hooks/gemini-cli/`. Track:
  <https://github.com/google-gemini/gemini-cli>.
- Source: <https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md>

---

## OpenClaw

**Status:** ✅ MCP supported. ⚠️ Hooks exist (TypeScript plugins,
not POSIX shell). Hook integration would need a small TS plugin in
`integrations/openclaw/`; not shipped here.

**Config file:** `~/.openclaw/config.json` (the OpenClaw docs reference
this path indirectly; verify with your `openclaw config show`).

OpenClaw distinguishes transports explicitly. Use
`"transport": "streamable-http"` for ai-memory's HTTP endpoint.

```json
{
  "mcp": {
    "servers": {
      "ai-memory": {
        "url": "http://127.0.0.1:49374/mcp",
        "transport": "streamable-http"
      }
    }
  }
}
```

**Gotchas:**
- OpenClaw config keys under `mcp.*` hot-reload without a gateway
  restart. Hook config changes do require restarting the gateway.
- A TypeScript hook plugin that mirrors the Claude Code hook bundle
  would let OpenClaw auto-capture sessions; until that exists, treat
  OpenClaw as MCP-only.
- Sources: <https://docs.openclaw.ai/cli/mcp>,
  <https://docs.openclaw.ai/automation/hooks>

---

## pi (not supported via MCP) {#pi-not-supported}

**Status:** ❌ MCP not supported upstream. Author Mario Zechner has
[explicitly stated](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
that MCP is not on pi's roadmap, citing the token-budget overhead of
typical MCP servers.

The upstream [agentmemory](https://github.com/rohitg00/agentmemory)
project wires pi via pi's own extension surface
(`~/.pi/agent/extensions/agentmemory/`), not MCP. Implementing that
for ai-memory is feasible but would require a separate pi-specific
plugin and is out of scope for ai-memory v0.2.

**What you can do today:**

1. **Best:** run ai-memory and use it from one of the MCP-capable
   clients listed above (Cursor, Claude Desktop, Gemini CLI,
   OpenClaw, or any of the flagship three in the README). Keep pi
   for the code-edit sessions that don't need cross-agent
   continuity. ai-memory's wiki dir is plain markdown; you can open
   it in any editor pi has access to.

2. **Unofficial:** there's a community [pi-mcp-adapter](https://github.com/nicobailon/pi-mcp-adapter)
   shim that bridges MCP into pi. Not endorsed by pi's author, may
   break on pi updates. If you try it, point it at ai-memory's
   `http://127.0.0.1:49374/mcp` endpoint.

3. **Future:** open an issue on
   <https://github.com/akitaonrails/ai-memory> requesting a native pi
   extension if there's enough demand. The work is straightforward
   (pi extensions are plain JS); just not prioritised today.

---

## After registering MCP - verify it works

Regardless of which client you used, the first sanity check is the
same: ask the model to list its available MCP tools, or to call
`memory_status` explicitly.

```
You: List the MCP tools you can call. Use one of them to check
     ai-memory's status.

Model (any client): I can call: memory_query, memory_recent,
     memory_status, memory_handoff_accept, memory_handoff_begin,
     memory_consolidate, memory_lint, memory_forget_sweep.
     memory_status reports: 0 pages, 0 observations, 0 sessions.
```

If the model doesn't see any of those tools, the MCP registration
isn't being picked up. Check:

1. **Is the server running?** `curl http://127.0.0.1:49374/mcp` should
   return a JSON-RPC error (not a connection refused). If refused,
   start ai-memory: `docker start ai-memory` or
   `ai-memory serve --transport http`.
2. **Did the client reload the config?** Cursor and Claude Desktop
   need a restart. Gemini CLI and OpenClaw usually pick it up on next
   session-start.
3. **Are you on the right port?** ai-memory's default is **49374**
   (`0xC0DE` in hex). If you remapped, update the URL in every
   client's config.

If the model sees the tools but they all error, the server is
probably running in a different data dir than expected. Check
`docker logs ai-memory` or `ai-memory status --json` for the data
dir on disk.

---

## When does the auto-handoff actually work?

The cross-agent handoff feature (the "headline" pitch in the README)
requires both sides - the agent that *ends* a session, and the agent
that *starts* the next one - to play nicely with ai-memory:

| Side | What's needed | Covered by |
|---|---|---|
| **Ending side** | The `SessionEnd` hook must fire ai-memory's `/hook` endpoint. | Built-in for Claude Code / Codex / OpenCode via the scripts in `hooks/`. |
| **Starting side** | Either (a) the `SessionStart` hook auto-injects the handoff via `/handoff`, OR (b) the model proactively calls `memory_handoff_accept` on first turn. | (a) is built-in for Claude Code / Codex / OpenCode. (b) works for any MCP-capable client if you nudge the model - see [the CLAUDE.md snippet](../README.md#nudging-the-agent-to-use-memory-proactively). |

So a typical mixed workflow looks like:

- **Claude Code → Cursor.** Claude Code's `SessionEnd` creates the
  handoff automatically. Cursor doesn't have a SessionStart hook, so
  in Cursor's first chat you nudge the model: "Call
  memory_handoff_accept to resume prior work." The model fetches the
  handoff via MCP and continues.
- **Claude Desktop → Claude Code.** Claude Desktop doesn't write a
  handoff (no hooks). To resume in Claude Code, you'd have had to
  call `memory_handoff_begin` manually in Claude Desktop before
  quitting. ai-memory's wiki content via `memory_query` is still
  available either way.
