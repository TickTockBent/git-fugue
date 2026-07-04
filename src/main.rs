//! gitfugue: procedural music from git repositories.
//!
//! Pipeline: extract -> analyze -> compose -> render (spec §3).

mod analyze;
mod compose;
mod compose_history;
mod extract;
mod model;
mod render;
mod rng;
mod theme;
mod theory;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use compose::ComposeParams;
use extract::{Extractor, ShellGit};
use model::{RepoModel, Scale};

#[derive(Parser)]
#[command(
    name = "gitfugue",
    version,
    about = "Procedural music from git repositories",
    long_about = "Run it in any repo, get a deterministic composition derived from \
                  the code and its history. Same input, byte-identical output."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Repository path (default: current directory)
    path: Option<PathBuf>,

    #[command(flatten)]
    shared: Shared,
}

#[derive(Subcommand)]
enum Command {
    /// The code at HEAD is the score (one rendering per tree state)
    Static {
        /// Repository path (default: current directory)
        path: Option<PathBuf>,
        #[command(flatten)]
        shared: Shared,
    },
    /// The commit history is the score (single voice; the fugue is Phase 3)
    History {
        /// Repository path (default: current directory)
        path: Option<PathBuf>,
        #[command(flatten)]
        shared: Shared,
        /// Commit range (e.g. v1.0..HEAD); default: last 300 commits
        #[arg(long)]
        range: Option<String>,
        /// No commit cap (large histories compress to one bar per K commits)
        #[arg(long)]
        full: bool,
    },
}

#[derive(Args)]
struct Shared {
    /// Output path; format inferred from extension (.mid | .wav)
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Play after rendering (Phase 4, not yet implemented)
    #[arg(long)]
    play: bool,

    /// Override the seed (hex), for exploration and debugging
    #[arg(long)]
    seed: Option<String>,

    /// pentatonic | minor-pentatonic | dorian | aeolian
    #[arg(long)]
    scale: Option<String>,

    /// Override base tempo
    #[arg(long)]
    bpm: Option<u16>,

    /// Output format when --out has no extension
    #[arg(long, default_value = "mid")]
    format: String,

    /// Print liner notes: the deterministic decision log
    #[arg(long, short)]
    verbose: bool,

    /// Target length in seconds (static mode; default 90-180 auto)
    #[arg(long)]
    duration: Option<u32>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("gitfugue: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // Bare `gitfugue` is history mode, per spec §7 (open question 6
        // stands: revisit after hearing both).
        None => {
            let repo = cli.path.unwrap_or_else(|| PathBuf::from("."));
            run_history(&repo, &cli.shared, None, false)
        }
        Some(Command::Static { path, shared }) => {
            let repo = path.unwrap_or_else(|| PathBuf::from("."));
            run_static(&repo, &shared)
        }
        Some(Command::History { path, shared, range, full }) => {
            let repo = path.unwrap_or_else(|| PathBuf::from("."));
            run_history(&repo, &shared, range.as_deref(), full)
        }
    }
}

/// Default range: the last 300 commits (spec §6.1). Anything larger
/// folds K commits per bar so `--full` on a 10k-commit repo is not a
/// six-hour piece.
const COMMIT_CAP: usize = 300;

fn run_history(repo: &Path, shared: &Shared, range: Option<&str>, full: bool) -> Result<()> {
    let scale = parse_scale(shared)?;

    // extract
    let git = ShellGit::open(repo)?;
    let root_hash = git.root_commit_hash()?;
    let identity_seed = match &shared.seed {
        Some(hex) => u64::from_str_radix(hex.trim_start_matches("0x"), 16)
            .context("--seed must be hex")?,
        None => u64::from_str_radix(&root_hash[..16.min(root_hash.len())], 16)
            .context("unexpected commit hash format")?,
    };
    let branch = git.head_branch()?;
    let total = git.first_parent_count()?;
    let limit = if full || range.is_some() {
        None
    } else {
        Some(COMMIT_CAP)
    };
    let raw = git.first_parent_log(range, limit)?;
    if raw.is_empty() {
        bail!("no commits in the selected range");
    }

    // analyze
    let compress = if raw.len() > 2 * COMMIT_CAP {
        raw.len().div_ceil(COMMIT_CAP) as u32
    } else {
        1
    };
    let model = analyze::build_history_model(&raw, identity_seed, branch, total, compress);

    // compose + render
    let params = ComposeParams {
        scale,
        bpm: shared.bpm,
        duration_secs: shared.duration,
    };
    let score = compose::compose(&model, &params);
    write_output(repo, shared, &score)
}

fn parse_scale(shared: &Shared) -> Result<Option<Scale>> {
    match &shared.scale {
        Some(s) => Ok(Some(Scale::parse(s).with_context(|| {
            format!("unknown scale '{s}' (pentatonic | minor-pentatonic | dorian | aeolian)")
        })?)),
        None => Ok(None),
    }
}

fn run_static(repo: &Path, shared: &Shared) -> Result<()> {
    let scale = parse_scale(shared)?;

    // extract
    let git = ShellGit::open(repo)?;
    let tree = git.head_tree_hash()?;
    let seed = match &shared.seed {
        Some(hex) => u64::from_str_radix(hex.trim_start_matches("0x"), 16)
            .context("--seed must be hex")?,
        None => u64::from_str_radix(&tree[..16.min(tree.len())], 16)
            .context("unexpected tree hash format")?,
    };
    let files = git.files_at_head()?;

    // analyze
    let units = analyze::build_units(&files);
    if units.is_empty() {
        bail!("no analyzable text files at HEAD");
    }
    let model = RepoModel::Static { seed, units };

    // compose
    let params = ComposeParams {
        scale,
        bpm: shared.bpm,
        duration_secs: shared.duration,
    };
    let score = compose::compose(&model, &params);
    write_output(repo, shared, &score)
}

fn write_output(repo: &Path, shared: &Shared, score: &model::Score) -> Result<()> {
    let out = output_path(repo, shared)?;
    let bytes = match out.extension().and_then(|e| e.to_str()) {
        Some("mid") | Some("midi") => render::render_midi(score)?,
        Some("wav") => bail!("WAV rendering is Phase 4 and not implemented yet; use .mid"),
        _ => bail!("unsupported output extension (use .mid)"),
    };
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    if shared.verbose {
        for line in &score.liner_notes {
            println!("{line}");
        }
    }
    let bars = score
        .events
        .iter()
        .map(|e| e.start + e.dur)
        .max()
        .unwrap_or(0)
        .div_ceil(compose::TPQ * 4);
    println!(
        "wrote {} ({} bars, {} {}, {} bpm, {} voices, seed {:016x})",
        out.display(),
        bars,
        score.key.name(),
        score.scale.name(),
        score.bpm_base,
        score.voices.len(),
        score.seed
    );
    if shared.play {
        eprintln!("note: --play is Phase 4 and not implemented yet");
    }
    Ok(())
}

fn output_path(repo: &Path, shared: &Shared) -> Result<PathBuf> {
    if let Some(out) = &shared.out {
        if out.extension().is_none() {
            return Ok(out.with_extension(&shared.format));
        }
        return Ok(out.clone());
    }
    let name = repo
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "gitfugue".to_string());
    match shared.format.as_str() {
        "mid" => Ok(PathBuf::from(format!("{name}.mid"))),
        "wav" => bail!("WAV rendering is Phase 4 and not implemented yet"),
        other => bail!("unknown format '{other}' (mid | wav)"),
    }
}
