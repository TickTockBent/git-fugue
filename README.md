# git fugue

Procedural music from git repositories. Run it in any repo, get a
deterministic composition derived from the code and its history. Same
input, byte-identical output — every repo has a signature song, and a
diff audibly changes it.

Full design: [gitfugue-spec.md](gitfugue-spec.md).

## Status

**Phase 3 (the fugue) is implemented.** The commit DAG is the score:

- Full DAG walk in topological order; branch = voice, assigned by a
  `git log --graph`-style lane allocator; trunk always holds voice 0
- Exposition: trunk states a 2–4 bar subject from the identity seed
  (root commit hash — the repo's sound is fixed at birth)
- Fork: the new voice copies its parent's theme state and enters
  transposed up a 5th or down a 4th, chosen by branch-name hash
  (names recovered from merge subjects)
- Commit = mutation: operators chosen by commit hash (interval nudge,
  displacement, ornament, note swap, rare inversion), magnitude by
  diff size — a branch's musical distance tracks its code distance
- Merge = cadence: both voices sound together for one bar, the target
  adopts a reconciled (interleaved) theme, the source releases;
  conflicted merges (detected via `git merge-tree`) get a suspension
  bar of tension first
- Voice cap (`--voices`, default 6) with least-recently-active
  eviction into a shared string-ensemble voice
- While one voice leads, other active voices comp softly underneath,
  consonance forced on downbeats
- Contributors: top authors get lead instruments (stable email-hash
  mapping) switched onto lanes per commit; bots play the hi-hat
- Timestamp deltas modulate local tempo ±15%; multi-day gaps insert a
  capped breath bar; `--verbose` liner notes name every entry, merge,
  conflict, and eviction

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

**Phase 4 (sound and polish) is implemented:**

- WAV rendering via rustysynth with an embedded TimGM6mb soundfont
  (~6 MB, GPL-2.0 — see assets/SOUNDFONT-LICENSE.md); `-o song.wav`
  or `--format wav` just works, `--soundfont` swaps in any SF2
- `--play` plays the rendered piece (build with
  `cargo build --features playback`; needs an audio backend such as
  ALSA headers on Linux)
- `.gitfugue.toml` at the repo root pins `scale` and `bpm`; CLI flags
  still win. Repos choose their own sound, and it ships with the code
- Perf pass: merge-tree conflict probes run on a thread pool
  (git/git's 300-commit window: 8.7s -> 3.9s, under the spec's 5s
  target)

Remaining from the spec: the Phase 4 stretch goals (`gitfugue watch`,
CI artifact mode) and the hybrid static+history mode (open question 1).

## Usage

```
gitfugue [PATH]              # history mode (spec default)
gitfugue static [PATH]       # the code at HEAD is the score
gitfugue history [PATH]      # the commit history is the score

Shared flags:
  -o, --out FILE        output path; format by extension (.mid | .wav)
      --format mid|wav  output format when --out is not given
      --play            play after rendering (playback-feature builds)
      --soundfont FILE  SF2 for WAV/playback (default: embedded TimGM6mb)
      --seed HEX        override the seed (exploration/debugging)
      --scale NAME      pentatonic | minor-pentatonic | dorian | aeolian
      --bpm N           override base tempo
  -v, --verbose         print liner notes (the deterministic decision log)

Static:
      --duration SECS   target length (default 90-180, chosen by seed)

History:
      --range A..B      commit range (default: last 300 commits)
      --branches LIST   restrict voices to these refs (comma-separated)
      --voices N        max simultaneous voices (default 6)
      --full            no commit cap (compresses to one bar per K commits)
```

Example:

```
$ gitfugue history . --verbose
key: E major pentatonic  bpm: 71  (identity from root commit)
range: 300 commits, 4 voices used (of 334 total commits)
voice 0: main (anchor, cello)
alice@example.com -> electric piano (233 commits)
bruno@example.com -> nylon guitar (9 commits)
7 more authors -> string ensemble (16 commits)
dependabot[bot] -> hi-hat (29 commits)
subject: 2 bars, stated by main
bar 3: voice 1 enters (feature/auth, down a 4th)
bar 27: merge feature/auth -> main, clean, cadence
bar 61: conflict tension
bar 62: merge hotfix -> main, conflicted, cadence
bar 141: 20-day gap, breath
wrote charlotte.mid (312 bars, E major pentatonic, 71 bpm, 6 voices, seed fb328a59a7272329)
```

## Building

```
cargo build --release                      # MIDI + WAV rendering
cargo build --release --features playback  # + --play (needs ALSA/CoreAudio)
cargo test                                 # includes the golden determinism tests
```

Repo config (committed alongside the code):

```toml
# .gitfugue.toml
scale = "minor-pentatonic"
bpm = 84
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
