# git fugue

Procedural music from git repositories. Run it in any repo, get a
deterministic composition derived from the code and its history. Same
input, byte-identical output — every repo has a signature song, and a
diff audibly changes it.

Full design: [gitfugue-spec.md](gitfugue-spec.md).

## Status

**Phase 2 (history mode, single voice) is implemented.** The commit
history is the score:

- First-parent walk of trunk; one commit = one bar (last 300 by
  default; `--full` folds K commits per bar on big histories)
- Identity seed from the root commit: key, scale, and tempo are fixed
  at the repo's birth — new commits extend the song without retconning it
- A 2–4 bar subject stated in an exposition, then mutated by every
  commit (operators chosen by commit hash, magnitude by diff size:
  interval nudge, displacement, ornament, note swap, rare inversion)
- Contributors: top authors get lead instruments (stable email-hash
  mapping), the long tail shares a string ensemble, and bots
  (dependabot / renovate / `*[bot]`) play the hi-hat
- Timestamp deltas modulate local tempo ±15%; long gaps insert a
  capped breath bar; merges get a low cadence strike
- `--verbose` liner notes name every voice and event

**Phase 1 (static mode) is implemented.** The code at HEAD is the score:

- Sorted walk of tracked files at HEAD (read from the HEAD tree, so a
  dirty working tree cannot change the song)
- Seed from the HEAD tree hash → key, scale, tempo, duration
- Top-level directories become sections, bar budget proportional to LOC
- Functions (tree-sitter for Rust/Python/JS, indentation fallback for
  everything else) become motifs; nesting depth drives tension
- Constrained composition: pentatonic/modal scales, contour templates,
  60/30/10 voice-leading with leap resolution, drone harmony, cadence
  figures between sections
- Standard MIDI file output with engine version + seed in the metadata
- Golden byte-identical determinism test in CI-able form

Not yet built: multi-voice fugue with branch lanes and merge cadences
(Phase 3), WAV/playback and `.gitfugue.toml` (Phase 4).

## Usage

```
gitfugue [PATH]              # history mode (spec default)
gitfugue static [PATH]       # the code at HEAD is the score
gitfugue history [PATH]      # the commit history is the score

Shared flags:
  -o, --out FILE        output path (default: <repo-name>.mid)
      --seed HEX        override the seed (exploration/debugging)
      --scale NAME      pentatonic | minor-pentatonic | dorian | aeolian
      --bpm N           override base tempo
  -v, --verbose         print liner notes (the deterministic decision log)

Static:
      --duration SECS   target length (default 90-180, chosen by seed)

History:
      --range A..B      commit range (default: last 300 commits)
      --full            no commit cap (compresses to one bar per K commits)
```

Example:

```
$ gitfugue history . --verbose
key: Bb major pentatonic  bpm: 82  (identity from root commit)
range: 300 first-parent commits on main (of 700 total)
alice@example.com -> flute (120 commits)
bob@example.com -> oboe (90 commits)
dependabot[bot] -> hi-hat (30 commits)
subject: 3 bars, stated by anchor
bar 47: merge 3f2c1ab
bar 112: 62-day gap, breath
wrote git-fugue.mid (304 bars, Bb major pentatonic, 82 bpm, 5 voices, seed 7e82feef21247d02)
```

## Building

```
cargo build --release
cargo test            # includes the golden determinism test
```

After an *intentional* engine change that alters output, bump the crate
version and regenerate the golden fixture:

```
UPDATE_GOLDEN=1 cargo test --test golden
```

## Architecture

Four-stage pipeline (spec §3); modes differ only in how they build the model.

```
extract  ->  analyze  ->  compose  ->  render
src/extract.rs  src/analyze.rs  src/compose.rs  src/render.rs
(git plumbing)  (RepoModel IR)  (all musical    (MIDI bytes)
                                 decisions)
```

- `src/model.rs` — the IRs: `RepoModel`, `CodeUnit`, `CommitNode`, `Score`
- `src/theory.rs` — the constraint layer: scales, contour templates,
  voice-leading walk. Data selects; theory constrains.
- `src/theme.rs` — the subject and its mutation operators (history mode)
- `src/compose_history.rs` — history-mode composer: exposition,
  commit bars, author timbres, tempo modulation
- `src/rng.rs` — SplitMix64, seeded hierarchically per scope. No
  hash-map iteration order ever feeds a musical choice.

Git access is behind the `Extractor` trait; the day-one implementation
shells out to `git` (`ls-tree` + one `cat-file --batch` process), to be
swapped for gix later without touching the pipeline.
