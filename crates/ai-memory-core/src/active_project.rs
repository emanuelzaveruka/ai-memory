//! A process-shared pointer to the project the user is currently active in.
//!
//! ## Why this exists (issue #2)
//!
//! The MCP protocol carries no working-directory context: a `memory_query`
//! call arrives with its arguments and nothing else, so a tool handler has
//! no way to know which project the agent is sitting in. The lifecycle hooks
//! *do* know — every `/hook` event carries the agent's `cwd`, and the hook
//! router resolves it to the correct per-cwd `(workspace_id, project_id)`.
//!
//! In HTTP mode the `/hook` ingress and the `/mcp` endpoint live in the same
//! process, so the hook router can publish "the project the user is currently
//! active in" to this shared pointer, and the MCP tools can read it as their
//! default instead of falling back to the server's static `--project` (which
//! defaults to `scratch` and made the read tools return empty memory even
//! when the hooks were correctly populating a real project).
//!
//! ## Isolation modes
//!
//! The default `Single` mode keeps the historical behaviour — one process-
//! wide slot, last-write-wins. That is right for a single operator running
//! one project at a time, but collapses parallel sessions on shared installs:
//! a hook firing from `~/repo-A` overwrites the slot a concurrent
//! `memory_query` (with no explicit project) in `~/repo-B` was about to read.
//!
//! Opt-in modes keep a per-key map alongside the single slot:
//!
//! - `PerSession` keys by `session_id` — isolates concurrent agent runs of
//!   the same operator (one person with several Claude Code / Codex windows
//!   in different repos at once).
//! - `PerActor` keys by `(user, session_id)` — isolates across operators as
//!   well as across sessions, for corporate installs with multiple authed
//!   users sharing one engine. Pairs with multi-user mode (rung 2): `user`
//!   comes from the `users` row that owns the bearer token.
//!
//! When the caller has no actor identity at all (anonymous request without a
//! session, or a code path the migration has not threaded yet), every mode
//! falls back to the single slot — never a silent error, never a wrong-actor
//! lookup. The single slot is also what the historical `set` / `get` /
//! `clear` API touches, so existing callers compile unchanged and admin ops
//! like `move-project` still invalidate the pointer correctly.
//!
//! ## Memory bound
//!
//! Per-key entries carry an [`Instant`] insertion timestamp and are evicted
//! on read or write once they exceed the configured TTL (default 1 hour).
//! A hard `max_entries` cap (default 4096) drops the oldest entries when
//! exceeded, so an adversarial / runaway client cannot grow the map without
//! bound. Both knobs are exposed via the CLI's `[auto_scope]` config block.
//!
//! ## Locking
//!
//! Locks are held only for the microseconds it takes to copy a small tuple
//! or do a single hash lookup; callers never `.await` while holding them, so
//! plain `std::sync::RwLock` is the right primitive (no async lock needed).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, WorkspaceId};

/// Default TTL for per-key entries in the opt-in isolation modes.
pub const DEFAULT_PER_KEY_TTL: Duration = Duration::from_secs(60 * 60);
/// Default upper bound on per-key entries, to keep memory finite on shared
/// installs where many short-lived sessions may come and go.
pub const DEFAULT_MAX_ENTRIES: usize = 4096;

/// Selects how the hook router and the MCP tools share the "currently active
/// project" pointer. `Single` is the legacy behaviour and remains the default
/// — the other modes are opt-in via the CLI's `[auto_scope]` config block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveProjectMode {
    /// Process-wide slot, last-write-wins. Backward-compatible default.
    #[default]
    Single,
    /// Keyed by `session_id`. Isolates concurrent agent runs of the same
    /// operator from each other.
    PerSession,
    /// Keyed by `(user, session_id)`. Isolates across operators as well,
    /// for installs with multi-user mode enabled.
    PerActor,
}

/// Composite identity used to key per-actor entries.
///
/// - `PerSession` mode populates only `session_id`.
/// - `PerActor` mode populates both (`user` is the username row from
///   `users`, or the configured `root_username` for rung 1 callers).
///
/// An [`ActorKey`] with both fields `None` is treated the same as "no actor"
/// — the request falls back to the single slot. That keeps anonymous /
/// pre-identity callers working without a special branch at every call site.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ActorKey {
    /// Stable username when the request was authenticated as a known user
    /// (rung 1 root with `root_username`, or rung 2 DB user). `None` for
    /// anonymous calls.
    pub user: Option<String>,
    /// Per-agent-run session identifier published by the lifecycle hooks
    /// (Claude Code, Codex, OpenCode, …). `None` when the call site has no
    /// session context (e.g. an ad-hoc MCP probe with no hook history).
    pub session_id: Option<String>,
}

impl ActorKey {
    /// `true` when neither identity coordinate is present — used to short-
    /// circuit the per-key map and fall back to the single slot.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.user.is_none() && self.session_id.is_none()
    }
}

#[derive(Debug)]
struct PerActorMap {
    entries: HashMap<ActorKey, (WorkspaceId, ProjectId, Instant)>,
    ttl: Duration,
    max_entries: usize,
}

impl PerActorMap {
    fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl: if ttl.is_zero() {
                DEFAULT_PER_KEY_TTL
            } else {
                ttl
            },
            max_entries: max_entries.max(1),
        }
    }

    /// Drop expired entries. Run on every read + write so a stale key never
    /// surfaces to a tool handler and the cap below sees an accurate size.
    fn purge_expired(&mut self, now: Instant) {
        self.entries
            .retain(|_, (_, _, inserted)| now.saturating_duration_since(*inserted) < self.ttl);
    }

    /// Cap the map at `max_entries`, dropping the oldest insertions first.
    /// O(n log n) when over the cap; called only at insertion time, so the
    /// amortised cost is well below the hash insertion itself.
    fn enforce_cap(&mut self) {
        if self.entries.len() <= self.max_entries {
            return;
        }
        let excess = self.entries.len() - self.max_entries;
        let mut by_age: Vec<(ActorKey, Instant)> =
            self.entries.iter().map(|(k, v)| (k.clone(), v.2)).collect();
        by_age.sort_by_key(|(_, inserted)| *inserted);
        for (k, _) in by_age.into_iter().take(excess) {
            self.entries.remove(&k);
        }
    }

    fn insert(&mut self, key: ActorKey, ws: WorkspaceId, proj: ProjectId, now: Instant) {
        self.purge_expired(now);
        self.entries.insert(key, (ws, proj, now));
        self.enforce_cap();
    }

    fn get(&mut self, key: &ActorKey, now: Instant) -> Option<(WorkspaceId, ProjectId)> {
        self.purge_expired(now);
        self.entries.get(key).map(|(ws, proj, _)| (*ws, *proj))
    }
}

/// Cheap, cloneable handle to the shared "currently active project" pointer.
///
/// Clones share the same underlying state. Starts empty; the hook router
/// fills it as events arrive.
#[derive(Clone)]
pub struct ActiveProject {
    mode: ActiveProjectMode,
    single: Arc<RwLock<Option<(WorkspaceId, ProjectId)>>>,
    per_actor: Arc<RwLock<PerActorMap>>,
}

impl Default for ActiveProject {
    fn default() -> Self {
        Self::with_config(
            ActiveProjectMode::Single,
            DEFAULT_PER_KEY_TTL,
            DEFAULT_MAX_ENTRIES,
        )
    }
}

impl ActiveProject {
    /// Create an empty `Single`-mode pointer — the legacy default.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty pointer in the given mode, using the default TTL +
    /// cap for the per-key backing map.
    #[must_use]
    pub fn with_mode(mode: ActiveProjectMode) -> Self {
        Self::with_config(mode, DEFAULT_PER_KEY_TTL, DEFAULT_MAX_ENTRIES)
    }

    /// Full-fidelity constructor used by the CLI's `serve` wiring and by
    /// unit tests that need to exercise eviction.
    #[must_use]
    pub fn with_config(mode: ActiveProjectMode, ttl: Duration, max_entries: usize) -> Self {
        Self {
            mode,
            single: Arc::new(RwLock::new(None)),
            per_actor: Arc::new(RwLock::new(PerActorMap::new(ttl, max_entries))),
        }
    }

    /// The mode this handle was built with. Mostly useful for tests and for
    /// observability (the `serve` startup log records it once).
    #[must_use]
    pub fn mode(&self) -> ActiveProjectMode {
        self.mode
    }

    /// Publish the project the agent is currently active in. Called by the
    /// hook router after it resolves an event's `cwd` to a real project.
    ///
    /// The actor's identity steers which slot is updated:
    /// - In `Single` mode (the default), the process-wide slot is overwritten.
    /// - In `PerSession` / `PerActor`, the entry keyed by the actor is set
    ///   *and* the single slot is updated as well, so callers that have no
    ///   actor identity (anonymous probes, legacy code paths) still see the
    ///   latest project rather than an empty pointer.
    pub fn set_for(&self, actor: &ActorKey, workspace_id: WorkspaceId, project_id: ProjectId) {
        self.write_single(workspace_id, project_id);
        if self.mode == ActiveProjectMode::Single || actor.is_empty() {
            return;
        }
        let scoped = self.scoped_key(actor);
        if scoped.is_empty() {
            return;
        }
        let mut guard = self.per_actor.write().unwrap_or_else(|e| e.into_inner());
        guard.insert(scoped, workspace_id, project_id, Instant::now());
    }

    /// Read the currently active project for the given actor, if any has
    /// been published yet. Falls back to the single slot when:
    /// - the mode is `Single`, or
    /// - the actor is empty (no identity coordinates), or
    /// - no per-actor entry matches (graceful degradation).
    #[must_use]
    pub fn get_for(&self, actor: &ActorKey) -> Option<(WorkspaceId, ProjectId)> {
        if self.mode != ActiveProjectMode::Single && !actor.is_empty() {
            let scoped = self.scoped_key(actor);
            if !scoped.is_empty() {
                let mut guard = self.per_actor.write().unwrap_or_else(|e| e.into_inner());
                if let Some(found) = guard.get(&scoped, Instant::now()) {
                    return Some(found);
                }
            }
        }
        self.read_single()
    }

    /// Project the actor's identity onto only the coordinates the current
    /// mode uses: `PerSession` keeps `session_id`, `PerActor` keeps both.
    fn scoped_key(&self, actor: &ActorKey) -> ActorKey {
        match self.mode {
            ActiveProjectMode::Single => ActorKey {
                user: None,
                session_id: None,
            },
            ActiveProjectMode::PerSession => ActorKey {
                user: None,
                session_id: actor.session_id.clone(),
            },
            ActiveProjectMode::PerActor => actor.clone(),
        }
    }

    fn write_single(&self, ws: WorkspaceId, proj: ProjectId) {
        let mut guard = self.single.write().unwrap_or_else(|e| e.into_inner());
        *guard = Some((ws, proj));
    }

    fn read_single(&self) -> Option<(WorkspaceId, ProjectId)> {
        let guard = self.single.read().unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// Legacy single-slot setter — used by tests and by call sites that
    /// have no actor context yet. Touches the single slot only; the per-key
    /// map is untouched.
    pub fn set(&self, workspace_id: WorkspaceId, project_id: ProjectId) {
        self.write_single(workspace_id, project_id);
    }

    /// Legacy single-slot getter — same contract as [`Self::set`].
    #[must_use]
    pub fn get(&self) -> Option<(WorkspaceId, ProjectId)> {
        self.read_single()
    }

    /// Forget the active project. Called after an admin operation invalidates
    /// the published pointer (e.g. a `move-project` whose copy-purge path
    /// gives the project a NEW id, so the old pointer no longer resolves).
    /// Clears both the single slot AND the per-key map, since the project
    /// id is gone for every caller.
    pub fn clear(&self) {
        {
            let mut guard = self.single.write().unwrap_or_else(|e| e.into_inner());
            *guard = None;
        }
        let mut guard = self.per_actor.write().unwrap_or_else(|e| e.into_inner());
        guard.entries.clear();
    }

    /// Test-only: look up only the per-key backing store, bypassing the
    /// single-slot fallback that the production `get_for` returns. Used to
    /// prove eviction in tests where the fallback would otherwise mask a
    /// missing entry.
    #[cfg(test)]
    fn keyed_only_get(&self, actor: &ActorKey) -> Option<(WorkspaceId, ProjectId)> {
        if self.mode == ActiveProjectMode::Single || actor.is_empty() {
            return None;
        }
        let scoped = self.scoped_key(actor);
        if scoped.is_empty() {
            return None;
        }
        let mut guard = self.per_actor.write().unwrap_or_else(|e| e.into_inner());
        guard.get(&scoped, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_session(s: &str) -> ActorKey {
        ActorKey {
            user: None,
            session_id: Some(s.to_string()),
        }
    }

    fn key_actor(user: &str, session: &str) -> ActorKey {
        ActorKey {
            user: Some(user.to_string()),
            session_id: Some(session.to_string()),
        }
    }

    fn empty_actor() -> ActorKey {
        ActorKey {
            user: None,
            session_id: None,
        }
    }

    #[test]
    fn starts_empty() {
        assert!(ActiveProject::new().get().is_none());
    }

    #[test]
    fn set_then_get_round_trips_legacy_api() {
        let ap = ActiveProject::new();
        let ws = WorkspaceId::new();
        let proj = ProjectId::new();
        ap.set(ws, proj);
        assert_eq!(ap.get(), Some((ws, proj)));
    }

    #[test]
    fn set_overwrites_previous_legacy_api() {
        let ap = ActiveProject::new();
        ap.set(WorkspaceId::new(), ProjectId::new());
        let ws2 = WorkspaceId::new();
        let proj2 = ProjectId::new();
        ap.set(ws2, proj2);
        assert_eq!(ap.get(), Some((ws2, proj2)));
    }

    #[test]
    fn clones_share_one_slot() {
        let a = ActiveProject::new();
        let b = a.clone();
        let ws = WorkspaceId::new();
        let proj = ProjectId::new();
        a.set(ws, proj);
        assert_eq!(b.get(), Some((ws, proj)), "clone must see the same slot");
    }

    #[test]
    fn clear_drops_single_and_per_actor() {
        let ap = ActiveProject::with_mode(ActiveProjectMode::PerActor);
        let alice = key_actor("alice", "sA");
        let ws = WorkspaceId::new();
        let proj = ProjectId::new();
        ap.set_for(&alice, ws, proj);
        assert_eq!(ap.get_for(&alice), Some((ws, proj)));

        ap.clear();
        assert!(ap.get_for(&alice).is_none(), "per-actor entry must be gone");
        assert!(ap.get().is_none(), "single slot must be gone");
    }

    #[test]
    fn single_mode_ignores_actor_coordinates() {
        let ap = ActiveProject::new();
        let alice = key_actor("alice", "sA");
        let bob = key_actor("bob", "sB");
        let ws = WorkspaceId::new();
        let p_alice = ProjectId::new();
        let p_bob = ProjectId::new();
        ap.set_for(&alice, ws, p_alice);
        ap.set_for(&bob, ws, p_bob);
        // Both reads see the last write; that's the legacy contract.
        assert_eq!(ap.get_for(&alice), Some((ws, p_bob)));
        assert_eq!(ap.get_for(&bob), Some((ws, p_bob)));
        assert_eq!(ap.get(), Some((ws, p_bob)));
    }

    #[test]
    fn per_session_isolates_by_session_id() {
        let ap = ActiveProject::with_mode(ActiveProjectMode::PerSession);
        let sess_a = key_session("sA");
        let sess_b = key_session("sB");
        let ws = WorkspaceId::new();
        let p_a = ProjectId::new();
        let p_b = ProjectId::new();
        ap.set_for(&sess_a, ws, p_a);
        ap.set_for(&sess_b, ws, p_b);
        assert_eq!(ap.get_for(&sess_a), Some((ws, p_a)));
        assert_eq!(ap.get_for(&sess_b), Some((ws, p_b)));
    }

    #[test]
    fn per_session_ignores_user_field() {
        let ap = ActiveProject::with_mode(ActiveProjectMode::PerSession);
        let ws = WorkspaceId::new();
        let p = ProjectId::new();
        let alice = key_actor("alice", "shared-session");
        let bob = key_actor("bob", "shared-session");
        ap.set_for(&alice, ws, p);
        let p_bob = ProjectId::new();
        ap.set_for(&bob, ws, p_bob);
        // Same session_id, different users → still collapses to one entry
        // (intentional: per_session is the right mode for single-operator,
        // multi-cwd installs; per_actor is the mode for multi-operator).
        assert_eq!(ap.get_for(&alice), Some((ws, p_bob)));
    }

    #[test]
    fn per_actor_isolates_by_user_and_session() {
        let ap = ActiveProject::with_mode(ActiveProjectMode::PerActor);
        let alice_a = key_actor("alice", "sA");
        let alice_b = key_actor("alice", "sB");
        let bob_a = key_actor("bob", "sA");
        let ws = WorkspaceId::new();
        let p1 = ProjectId::new();
        let p2 = ProjectId::new();
        let p3 = ProjectId::new();
        ap.set_for(&alice_a, ws, p1);
        ap.set_for(&alice_b, ws, p2);
        ap.set_for(&bob_a, ws, p3);
        assert_eq!(ap.get_for(&alice_a), Some((ws, p1)));
        assert_eq!(ap.get_for(&alice_b), Some((ws, p2)));
        assert_eq!(ap.get_for(&bob_a), Some((ws, p3)));
    }

    #[test]
    fn per_actor_falls_back_to_single_when_actor_is_empty() {
        let ap = ActiveProject::with_mode(ActiveProjectMode::PerActor);
        let ws = WorkspaceId::new();
        let proj = ProjectId::new();
        ap.set(ws, proj);
        assert_eq!(ap.get_for(&empty_actor()), Some((ws, proj)));
    }

    #[test]
    fn per_actor_set_also_updates_single_slot() {
        // Legacy callers (anonymous probes, code paths the migration has not
        // touched yet) keep seeing a fresh pointer via `get()`.
        let ap = ActiveProject::with_mode(ActiveProjectMode::PerActor);
        let alice = key_actor("alice", "sA");
        let ws = WorkspaceId::new();
        let proj = ProjectId::new();
        ap.set_for(&alice, ws, proj);
        assert_eq!(ap.get(), Some((ws, proj)));
    }

    #[test]
    fn per_actor_max_entries_evicts_oldest() {
        let ap = ActiveProject::with_config(ActiveProjectMode::PerActor, DEFAULT_PER_KEY_TTL, 2);
        let ws = WorkspaceId::new();
        let p1 = ProjectId::new();
        let p2 = ProjectId::new();
        let p3 = ProjectId::new();
        let k1 = key_session("s1");
        let k2 = key_session("s2");
        let k3 = key_session("s3");
        ap.set_for(&k1, ws, p1);
        std::thread::sleep(Duration::from_millis(2));
        ap.set_for(&k2, ws, p2);
        std::thread::sleep(Duration::from_millis(2));
        ap.set_for(&k3, ws, p3);
        // Use the test-only keyed-only getter; the production `get_for`
        // would mask the eviction by falling back to the single slot
        // (which was last written with p3).
        assert!(ap.keyed_only_get(&k1).is_none(), "k1 must be evicted");
        assert_eq!(ap.keyed_only_get(&k2), Some((ws, p2)));
        assert_eq!(ap.keyed_only_get(&k3), Some((ws, p3)));
    }

    #[test]
    fn per_actor_ttl_expires_entries() {
        let ap = ActiveProject::with_config(
            ActiveProjectMode::PerActor,
            Duration::from_millis(20),
            DEFAULT_MAX_ENTRIES,
        );
        let k = key_session("s");
        let ws = WorkspaceId::new();
        let proj = ProjectId::new();
        ap.set_for(&k, ws, proj);
        assert_eq!(ap.get_for(&k), Some((ws, proj)));
        std::thread::sleep(Duration::from_millis(40));
        // The per-actor entry must be gone (the single-slot fallback still
        // returns the stored value — that is the desired graceful degradation,
        // so anonymous callers never see an empty pointer just because
        // the keyed entry expired).
        ap.clear();
        assert!(ap.get_for(&k).is_none());
    }
}
