//! gitfugue: procedural music from git repositories.
//!
//! Pipeline: extract -> analyze -> compose -> render (spec §3).

mod analyze;
mod compose;
mod extract;
mod model;
mod render;
mod rng;
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
    /// The commit DAG is the score (Phase 2, not yet implemented)
    History {
        /// Repository path (default: current directory)
        path: Option<PathBuf>,
        #[command(flatten)]
        shared: Shared,
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
    let (mode, path, shared) = match cli.command {
        // Bare `gitfugue` defaults to static for now: it is the mode
        // that exists. Spec open question 6 revisits this in Phase 2.
        None => ("static", cli.path, cli.shared),
        Some(Command::Static { path, shared }) => ("static", path, shared),
        Some(Command::History { .. }) => {
            bail!(
                "history mode is Phase 2 and not implemented yet; \
                 try `gitfugue static` for now"
            );
        }
    };
    let repo = path.unwrap_or_else(|| PathBuf::from("."));
    match mode {
        "static" => run_static(&repo, &shared),
        _ => unreachable!(),
    }
}

fn run_static(repo: &Path, shared: &Shared) -> Result<()> {
    let scale = match &shared.scale {
        Some(s) => Some(Scale::parse(s).with_context(|| {
            format!("unknown scale '{s}' (pentatonic | minor-pentatonic | dorian | aeolian)")
        })?),
        None => None,
    };

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

    // render
    let out = output_path(repo, shared)?;
    let bytes = match out.extension().and_then(|e| e.to_str()) {
        Some("mid") | Some("midi") => render::render_midi(&score)?,
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
