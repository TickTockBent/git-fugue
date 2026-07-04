# git fugue

Procedural music from git repositories. Run it in any repo, get a
deterministic composition derived from the code and its history. Same
input, byte-identical output — every repo has a signature song, and a
diff audibly changes it.

Full design: [gitfugue-spec.md](gitfugue-spec.md).

## Status

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

Not yet built: history mode (Phase 2), the fugue (Phase 3), WAV/playback
and `.gitfugue.toml` (Phase 4).

## Usage

```
gitfugue [PATH]              # static mode on the repo (history mode is Phase 2)
gitfugue static [PATH]

  -o, --out FILE        output path (default: <repo-name>.mid)
      --seed HEX        override the seed (exploration/debugging)
      --scale NAME      pentatonic | minor-pentatonic | dorian | aeolian
      --bpm N           override base tempo
      --duration SECS   target length (default 90-180, chosen by seed)
  -v, --verbose         print liner notes (the deterministic decision log)
```

Example:

```
$ gitfugue static . --verbose
key: D major pentatonic  bpm: 96  target: 99s (39 bars)
section src: 24 bars (1420 loc, 7 files)
section tests: 8 bars (310 loc, 2 files)
voice 0: cello
voice 1: pizzicato strings
wrote git-fugue.mid (39 bars, D major pentatonic, 96 bpm, 3 voices, seed 75455d634a453651)
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

- `src/model.rs` — the IRs: `RepoModel`, `CodeUnit`, `Score`
- `src/theory.rs` — the constraint layer: scales, contour templates,
  voice-leading walk. Data selects; theory constrains.
- `src/rng.rs` — SplitMix64, seeded hierarchically per scope. No
  hash-map iteration order ever feeds a musical choice.

Git access is behind the `Extractor` trait; the day-one implementation
shells out to `git` (`ls-tree` + one `cat-file --batch` process), to be
swapped for gix later without touching the pipeline.
