//! gitfugue: procedural music from git repositories.
//!
//! Pipeline: extract -> analyze -> compose -> render (spec §3).

mod analyze;
mod compose;
mod compose_history;
mod config;
mod extract;
mod model;
mod render;
mod rng;
mod synth;
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
    /// The commit DAG is the score: branches are voices in a fugue
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
        /// Restrict voices to these branches (comma-separated refs)
        #[arg(long, value_delimiter = ',')]
        branches: Vec<String>,
        /// Max simultaneous voices
        #[arg(long, default_value_t = 6)]
        voices: u8,
    },
}

struct HistoryOpts<'a> {
    range: Option<&'a str>,
    full: bool,
    branches: &'a [String],
    voices: u8,
}

#[derive(Args)]
struct Shared {
    /// Output path; format inferred from extension (.mid | .wav)
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Play after rendering (needs a build with the `playback` feature)
    #[arg(long)]
    play: bool,

    /// SF2 soundfont for WAV rendering/playback (default: embedded TimGM6mb)
    #[arg(long)]
    soundfont: Option<PathBuf>,

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
            let opts = HistoryOpts { range: None, full: false, branches: &[], voices: 6 };
            run_history(&repo, &cli.shared, &opts)
        }
        Some(Command::Static { path, shared }) => {
            let repo = path.unwrap_or_else(|| PathBuf::from("."));
            run_static(&repo, &shared)
        }
        Some(Command::History { path, shared, range, full, branches, voices }) => {
            let repo = path.unwrap_or_else(|| PathBuf::from("."));
            let opts = HistoryOpts {
                range: range.as_deref(),
                full,
                branches: &branches,
                voices,
            };
            run_history(&repo, &shared, &opts)
        }
    }
}

/// Default range: the last 300 commits (spec §6.1). Anything larger
/// folds K commits per bar so `--full` on a 10k-commit repo is not a
/// six-hour piece.
const COMMIT_CAP: usize = 300;

fn run_history(repo: &Path, shared: &Shared, opts: &HistoryOpts) -> Result<()> {
    // extract
    let git = ShellGit::open(repo)?;
    let cfg = config::load(git.root())?;
    let scale = parse_scale(shared)?.or(cfg.scale);
    let bpm = shared.bpm.or(cfg.bpm);
    let root_hash = git.root_commit_hash()?;
    let identity_seed = match &shared.seed {
        Some(hex) => u64::from_str_radix(hex.trim_start_matches("0x"), 16)
            .context("--seed must be hex")?,
        None => u64::from_str_radix(&root_hash[..16.min(root_hash.len())], 16)
            .context("unexpected commit hash format")?,
    };
    let branch = git.head_branch()?;
    let total = git.commit_count()?;
    let limit = if opts.full || opts.range.is_some() {
        None
    } else {
        Some(COMMIT_CAP)
    };
    let raw = git.log(opts.range, limit, opts.branches, false)?;
    if raw.is_empty() {
        bail!("no commits in the selected range");
    }

    // analyze
    let model = if raw.len() > 2 * COMMIT_CAP {
        // Huge history: fold K commits per bar on the first-parent
        // walk; fugal entries are inaudible at K:1 anyway.
        let compress = raw.len().div_ceil(COMMIT_CAP) as u32;
        let fp = git.log(opts.range, None, opts.branches, true)?;
        analyze::build_folded_model(&fp, identity_seed, branch, total, compress)
    } else {
        // Conflict probe per merge (spec §6.2): merge-tree between
        // parents, degrading to "clean" on any error. Probes are
        // independent git processes, so run them on a thread pool;
        // results land by index, keeping output deterministic.
        let conflicted = probe_conflicts(&git, &raw);
        let head = git.head_commit()?;
        analyze::build_history_model(
            &raw,
            identity_seed,
            branch,
            total,
            &head,
            opts.voices,
            &conflicted,
        )
    };

    // compose + render
    let params = ComposeParams {
        scale,
        bpm,
        duration_secs: shared.duration,
    };
    let score = compose::compose(&model, &params);
    write_output(repo, shared, &score)
}

/// Run `git merge-tree` for every merge commit, fanned out over a few
/// worker threads (each probe forks a git process; on an 81k-commit
/// repo a sequential pass dominated render time 8:1).
fn probe_conflicts(git: &ShellGit, raw: &[extract::RawCommit]) -> Vec<bool> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let jobs: Vec<usize> = (0..raw.len()).filter(|&i| raw[i].parents.len() > 1).collect();
    let results: Vec<std::sync::atomic::AtomicBool> =
        (0..raw.len()).map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
    let cursor = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
        .min(jobs.len().max(1));

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let j = cursor.fetch_add(1, Ordering::Relaxed);
                if j >= jobs.len() {
                    break;
                }
                let i = jobs[j];
                let c = &raw[i];
                if git.merge_conflicted(&c.parents[0], &c.parents[1]) {
                    results[i].store(true, Ordering::Relaxed);
                }
            });
        }
    });
    results.into_iter().map(|b| b.into_inner()).collect()
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
    // extract
    let git = ShellGit::open(repo)?;
    let cfg = config::load(git.root())?;
    let scale = parse_scale(shared)?.or(cfg.scale);
    let bpm = shared.bpm.or(cfg.bpm);
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
        bpm,
        duration_secs: shared.duration,
    };
    let score = compose::compose(&model, &params);
    write_output(repo, shared, &score)
}

fn write_output(repo: &Path, shared: &Shared, score: &model::Score) -> Result<()> {
    let out = output_path(repo, shared)?;
    let midi = render::render_midi(score)?;
    let soundfont = shared.soundfont.as_deref();
    let bytes = match out.extension().and_then(|e| e.to_str()) {
        Some("mid") | Some("midi") => midi.clone(),
        Some("wav") => synth::render_wav(&midi, soundfont)?,
        _ => bail!("unsupported output extension (use .mid or .wav)"),
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
        let (left, right) = synth::synthesize(&midi, soundfont)?;
        if let Err(e) = synth::play(left, right) {
            // The file is already on disk; a missing audio backend
            // should not turn a successful render into a failure.
            eprintln!("gitfugue: {e}");
        }
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
        "mid" | "wav" => Ok(PathBuf::from(format!("{name}.{}", shared.format))),
        other => bail!("unknown format '{other}' (mid | wav)"),
    }
}
