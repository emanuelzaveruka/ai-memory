//! `ai-memory bootstrap` — ingest an existing project's history.
//!
//! Thin CLI wrapper around `ai_memory_consolidate::Bootstrap`. The
//! heavy lifting (source collection, prioritisation, LLM call,
//! wiki write) lives in the consolidate crate; this file just
//! resolves CLI args into a `BootstrapConfig` and dispatches.
//!
//! Resolves the repo path via `git rev-parse --show-toplevel` when
//! `--repo-path` is omitted, so running from any subdirectory of
//! the target project works.

use std::path::PathBuf;
use std::sync::Arc;

use ai_memory_consolidate::{Bootstrap, BootstrapConfig, BootstrapOutcome};
use ai_memory_llm::{build_provider, provider_from_env};
use ai_memory_store::Store;
use ai_memory_wiki::Wiki;
use anyhow::{Context, Result, bail};
use tracing::info;

use crate::cli::BootstrapArgs;
use crate::config::Config;

/// Run the `bootstrap` subcommand.
///
/// # Errors
/// Bails when an LLM provider isn't configured (bootstrap can't run
/// without one), when the resolved repo path isn't a git repo, when
/// the project was already bootstrapped without `--force`, or when
/// any source-collection / LLM / wiki write fails.
pub async fn run(config: &Config, args: BootstrapArgs) -> Result<()> {
    // ---- LLM provider — required ----
    let Some(llm_cfg) = provider_from_env()? else {
        bail!(
            "bootstrap requires an LLM provider. Set AI_MEMORY_LLM_PROVIDER \
             (and the matching API key) and try again."
        );
    };
    let llm = build_provider(llm_cfg).context("building LLM provider from env")?;
    info!(
        provider = llm.name(),
        model = llm.model(),
        "bootstrap LLM enabled"
    );

    // ---- repo path — auto-detect if absent ----
    let repo_path = match args.repo_path {
        Some(p) => p,
        None => resolve_repo_root().context("auto-detecting --repo-path via git rev-parse")?,
    };
    if !repo_path.join(".git").exists() {
        bail!(
            "repo path {} is not a git repository (looked for {}/.git)",
            repo_path.display(),
            repo_path.display()
        );
    }

    // ---- open store + wiki ----
    let store = Store::open(&config.data_dir)
        .with_context(|| format!("opening store at {}", config.data_dir.display()))?;
    let ws = store
        .writer
        .get_or_create_workspace(args.workspace.clone())
        .await?;
    let proj = store
        .writer
        .get_or_create_project(ws, args.project.clone(), None)
        .await?;
    let wiki = Wiki::new(&config.data_dir, store.writer.clone())?;

    // ---- run bootstrap ----
    let cfg = BootstrapConfig {
        repo_path: repo_path.clone(),
        workspace_id: ws,
        project_id: proj,
        max_input_tokens: args.max_input_tokens,
        include_git: !args.exclude_git,
        include_readme: !args.exclude_readme,
        include_docs: !args.exclude_docs,
        include_code: !args.exclude_code,
        since: args.since,
        dry_run: args.dry_run,
        force: args.force,
    };
    let bootstrap = Bootstrap {
        reader: store.reader.clone(),
        wiki,
        llm: Arc::clone(&llm),
    };
    let outcome = bootstrap.run(&cfg).await?;
    print_human_report(&outcome, &args.workspace, &args.project);
    // Also emit the machine-readable JSON at the end for scripted callers.
    let report = serde_json::to_string_pretty(&outcome)?;
    println!("\n--- machine-readable ---\n{report}");
    Ok(())
}

/// Render the bootstrap outcome as a human-friendly summary. Lists
/// each source kind separately + every page written + an explicit
/// "what ai-memory knows now" footer so the operator doesn't assume
/// the wiki has 100% coverage of the project.
fn print_human_report(outcome: &BootstrapOutcome, workspace: &str, project: &str) {
    let kind = if outcome.dry_run {
        "Dry-run"
    } else {
        "Bootstrap"
    };
    println!("\n✓ {kind} complete for {workspace}/{project}\n");

    println!("Sources loaded into the LLM:");
    let c = &outcome.sources_by_kind;
    if c.git_commits > 0 {
        println!(
            "  - {} git commit summar{}",
            c.git_commits,
            if c.git_commits == 1 { "y" } else { "ies" }
        );
    }
    if c.readme > 0 {
        println!("  - README");
    }
    if c.doc_files > 0 {
        println!(
            "  - {} doc file{} (under docs/)",
            c.doc_files,
            if c.doc_files == 1 { "" } else { "s" }
        );
    }
    if c.module_headers > 0 {
        println!(
            "  - {} Rust module header{}",
            c.module_headers,
            if c.module_headers == 1 { "" } else { "s" }
        );
    }
    if c.project_rules > 0 {
        println!(
            "  - {} project-rules file{} (CLAUDE.md / AGENTS.md / …)",
            c.project_rules,
            if c.project_rules == 1 { "" } else { "s" }
        );
    }
    println!(
        "  → ~{} input tokens estimated{}",
        outcome.estimated_input_tokens,
        if outcome.sources_dropped > 0 {
            format!(
                " (dropped {} lower-priority source{} to stay under budget)",
                outcome.sources_dropped,
                if outcome.sources_dropped == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else {
            String::new()
        }
    );

    if outcome.dry_run {
        println!("\n(dry-run — no LLM call, no pages written)");
    } else {
        println!(
            "\nGenerated {} wiki page{}:",
            outcome.pages_written.len(),
            if outcome.pages_written.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for p in &outcome.pages_written {
            println!("  - {p}");
        }
        if !outcome.rationale.is_empty() {
            println!("\nRationale: {}", outcome.rationale);
        }
    }

    println!(
        "\n⚠ What ai-memory knows now\n  \
         Only the sources listed above. NOT every file in your project,\n  \
         NOT every commit since project start, NOT runtime behaviour or\n  \
         test logs. As you use Claude Code (or another MCP agent) the\n  \
         lifecycle hooks will automatically capture your actual workflow,\n  \
         and consolidation will refine the wiki over time."
    );
}

/// `git rev-parse --show-toplevel` — finds the repo root from $PWD.
fn resolve_repo_root() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running `git rev-parse --show-toplevel`")?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed (cwd is not inside a git repository?). \
             stderr: {}",
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(line))
}
