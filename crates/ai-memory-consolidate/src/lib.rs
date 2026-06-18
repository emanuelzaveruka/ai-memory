//! Karpathy "LLM Wiki" consolidation pipeline.
//!
//! M7a delivers the single-page variant: rewrite one
//! `sessions/<id>.md` page from raw observations via an LLM. The
//! store's sha256-equality short-circuit + supersession chain means
//! the rewrite is a *version*, not a destructive overwrite —
//! exactly the Karpathy pattern.
//!
//! M7b extends this to multi-page atomic fan-out.

pub mod auto_improve;
pub mod bootstrap;
pub mod consolidator;
pub mod curator;
pub mod lint;
pub mod projection;
pub mod sweep;
pub mod types;

pub use auto_improve::{
    AutoImproveError, AutoImproveEvidence, AutoImproveLlmResponse, AutoImproveProposal,
    AutoImproveRejectedCandidate, AutoImproveReport, AutoImproveReviewConfig,
    DEFAULT_AUTO_IMPROVE_MAX_INPUT_TOKENS, DEFAULT_AUTO_IMPROVE_MAX_PROPOSALS,
    DEFAULT_AUTO_IMPROVE_MIN_CONFIDENCE, DEFAULT_AUTO_IMPROVE_MIN_OBSERVATIONS,
    DEFAULT_AUTO_IMPROVE_MIN_SESSION_DURATION_SECS, DEFAULT_AUTO_IMPROVE_PENDING_PATH,
    DEFAULT_AUTO_IMPROVE_PROPOSAL_ACTOR, run_auto_improve_review,
};
pub use bootstrap::{
    Bootstrap, BootstrapConfig, BootstrapError, BootstrapOutcome, BootstrapSource,
    DEFAULT_CHUNK_INPUT_TOKENS, ProjectNameStrategy, SourceCounts, SourceKind, collect_sources,
    derive_project_name, discover_main_repo_root, discover_repo_root, effective_chunk_budget,
    plan_bootstrap_chunks, prune_sources_to_budget,
};
pub use consolidator::{
    BATCH_SYSTEM_PROMPT, Consolidator, ConsolidatorError, ConsolidatorResult, build_batch_request,
};
pub use curator::{
    CuratorFinding, CuratorParams, CuratorReport, render_curator_report_markdown,
    run_curator_report,
};
pub use lint::{LintError, LintFinding, LintReport, run_lint};
pub use sweep::{EvictedPage, SweepError, SweepReport, run_sweep};
pub use types::{
    ConsolidatedBatch, ConsolidatedPage, ConsolidatedPageUpdate, ConsolidationOutcome, PageKind,
    SlotKind,
};
