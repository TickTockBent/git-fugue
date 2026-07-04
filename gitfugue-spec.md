# git fugue

Procedural music from git repositories. Run it in any repo, get a deterministic composition derived from the code and its history.

**Spec v0.1** (draft, pre-build)

---

## 1. Concept

A single-binary CLI tool. Two modes:

1. **Static mode**: the code at HEAD is the score. One rendering per tree state.
2. **History mode**: the commit DAG is the score. Branches are voices in a fugue, commits are mutations of a theme, merges are cadences, contributors are instruments.

Core design law: **data never maps directly to notes.** Data makes choices inside a system that is already constrained to be musical. The theory layer guarantees it sounds like music; the data layer guarantees it sounds like *this repo*.

## 2. Design principles

1. **Determinism.** Same input produces byte-identical output. Every repo has a signature song. A diff audibly changes it.
2. **Constrained musicality.** All randomness resolves inside pre-validated structures: scale, rhythmic grid, voice-leading rules, contour templates. Data selects; theory constrains.
3. **Honest signal.** Musical features correspond to real repo features. Divergence sounds like divergence. A bus-factor-one repo sounds like a solo. The tool never fakes richness that is not in the data.
4. **Zero config to first sound.** `gitfugue` with no arguments in a repo produces output.

## 3. Architecture

Four-stage pipeline. The composition engine is shared; modes differ only in how they build the model.

```
extract  ->  analyze  ->  compose  ->  render
(git,        (RepoModel    (Score:      (MIDI file,
tree-sitter)  IR)           voices +     optional WAV,
                            events)      optional playback)
```

- **Extract**: git plumbing + tree-sitter parsing. Produces raw facts.
- **Analyze**: normalize facts into a `RepoModel` (mode-specific).
- **Compose**: deterministic mapping `RepoModel -> Score`. All musical decisions happen here.
- **Render**: `Score -> .mid` always; `.wav` via embedded synth; live playback optional.

### 3.1 Intermediate representations

```rust
// Mode-specific input to the composer
enum RepoModel {
    Static {
        seed: u64,                  // from HEAD tree hash
        units: Vec<CodeUnit>,       // files/functions with metrics
    },
    History {
        identity_seed: u64,         // from root commit hash
        commits: Vec<CommitNode>,   // topo-ordered DAG
        authors: Vec<Author>,
    },
}

struct CodeUnit {
    path: PathBuf,
    depth: u8,           // directory depth
    language: Lang,
    loc: u32,
    nesting_max: u8,     // complexity proxy
    ident_hash: u64,     // rolling hash of identifiers
    functions: Vec<FnUnit>,
}

struct CommitNode {
    hash: [u8; 20],
    parents: Vec<usize>,
    author_id: usize,
    timestamp: i64,
    diff_magnitude: u32,     // insertions + deletions
    branch_lane: u8,         // assigned by lane allocator
    is_merge: bool,
}

// Mode-agnostic output of the composer
struct Score {
    bpm_base: u16,
    key: Key,
    scale: Scale,
    voices: Vec<Voice>,          // voice = channel + instrument
    events: Vec<NoteEvent>,      // pitch, start_tick, dur, velocity, voice
    tempo_map: Vec<(Tick, u16)>,
    liner_notes: Vec<String>,    // human-readable decision log
}
```

## 4. Musical constraint system

The theory layer. Applies to both modes.

| Element | Rule |
|---|---|
| Scale | Default major pentatonic. Config: minor pentatonic, dorian, aeolian. All pitches quantize to scale. |
| Grid | 4/4, 16th-note resolution. Base BPM seeded from range 70 to 110. |
| Register | 3-octave usable range per voice, clamped. |
| Voice leading | Interval choice weighted: 60% step, 30% third, 10% larger leap. Leaps resolve by step in the opposite direction. |
| Phrases | Small library of contour templates: arch, ramp, valley, plateau, zigzag. Data selects template and parameters. |
| Tension `t` in [0,1] | Scalar per phrase/bar. Modulates: note density, velocity variance, register spread, and (at high t) permitted non-scale passing/neighbor tones. High t may borrow the parallel minor. |
| Harmony | Static mode: drone/pad root + fifth under melody. History mode: counterpoint rules force consonant intervals between voices on strong beats. |

### 4.1 Seed derivation

- **Static mode**: `seed = SHA(HEAD tree)`. Any content change changes the song. This is the point.
- **History mode**: global parameters (key, scale, base BPM, palette) seed from the **root commit hash**. The repo's identity is fixed at birth; new commits extend the song without retconning its foundation. Per-commit mutations seed from each commit's own hash.
- Single PRNG family (ChaCha8 or SplitMix64). No `HashMap` iteration order ever feeds a musical choice; all collections sorted or BTree.

## 5. Mode 1: static

Deterministic sorted walk of tracked files at HEAD (`git ls-files`, so .gitignore is respected for free).

### 5.1 Mapping table

| Repo feature | Musical parameter |
|---|---|
| HEAD tree hash | Seed: key, scale, BPM, instrument palette |
| Top-level directory | Section (with a transition figure between sections) |
| Directory depth | Octave within voice range |
| File language | Instrument (see palette table) |
| File | Phrase group |
| Function / class (tree-sitter) | Motif |
| Function length | Phrase length, clamped 1 to 4 bars |
| Max nesting depth | Tension `t` (proxy for cyclomatic complexity; cheap and language-agnostic) |
| Identifier hash stream | Contour template selection + scale-degree choices |
| Directory LOC weight | Bar budget allocation per section |

### 5.2 Duration budgeting

Target length defaults to 90 to 180 seconds regardless of repo size. Allocate bars to top-level directories proportional to LOC weight, then deterministically sample representative units within each directory when over budget (sort by a stable key, stride-sample). `--duration` overrides. A 10k-file repo must not produce a 4-hour piece by default.

### 5.3 Language fallback

If no tree-sitter grammar matches: indentation-based block detection supplies function boundaries and nesting depth. Guarantees any text repo renders.

### 5.4 Starter instrument palette

Tunable, but concrete defaults matter. General MIDI programs:

| Language family | Instrument |
|---|---|
| Rust / C / C++ | Cello / low strings |
| Python | Acoustic piano |
| JS / TS | Electric piano |
| Go | Marimba |
| Java / Kotlin / C# | French horn |
| HTML / CSS | Warm pad |
| Markdown / docs | Ambient pad (background) |
| YAML / TOML / JSON | Pizzicato strings |
| Shell / CI config | Light percussion |

## 6. Mode 2: history

### 6.1 Timeline

- Topological order of the commit DAG is the beat grid. Default: **one commit = one bar.**
- `diff_magnitude` (log-scaled, clamped) sets note density within the bar.
- Timestamp deltas modulate local tempo plus or minus 15% and insert breath rests for long gaps. Gaps are capped: never literal silence proportional to wall-clock time.
- Default range: last 300 commits of the current branch's history unless `--full` or `--range` given. (One commit per bar at ~2s/bar means 10k commits is a 6-hour piece. Cap by default, compress on demand: at large N, `--full` switches to one bar per K commits.)

### 6.2 Voices and the fugue structure

- **Branch = voice.** Lane assignment uses the same lane-allocation algorithm as `git log --graph`. Max simultaneous voices default 6; overflow branches fold into a shared ensemble voice, least-recently-active first.
- **Trunk is the anchor.** main/master always holds voice 0 and the anchor instrument.
- **Exposition**: the piece opens with trunk stating the subject, a 2-to-4-bar theme generated from the identity seed.
- **Fork**: the new voice copies the parent's current theme state, waits one bar, then enters with the subject transposed by a consonant interval (4th or 5th, chosen by branch-name hash). Classic fugal entry.
- **Commit = mutation.** Operator chosen by commit hash from: interval nudge, rhythmic displacement, ornament insert/remove, in-scale note swap, contour inversion (rare). Magnitude weighted by diff size. A branch's musical distance from trunk tracks its code distance.
- **Merge = cadence.** Both voices sound together for one bar (counterpoint rules enforce consonance on strong beats), then the target voice adopts a reconciled theme (elements interleaved from both parents, seeded by the merge commit hash) and the source voice releases.
- **Conflicted merges** (Phase 3+): recompute with `git merge-tree` between parents; if conflicts existed, insert one bar of heightened tension (t spike, suspension figure) before resolution.

### 6.3 Contributors

- Normalized author email hashes to an instrument, stable across renders.
- Top-N authors by commit count get distinct lead instruments; the long tail shares a section instrument.
- A branch voice plays in the timbre of each commit's author, so a two-person branch audibly alternates instruments on one melodic line.
- **Bot detection** (`dependabot`, `renovate`, `*[bot]`): routed to the percussion channel. Dependabot's near-regular cadence makes it the hi-hat. This is a feature, not a joke. (It is also a joke.)

## 7. CLI

```
gitfugue [PATH]              # default: history mode, render + play if possible
gitfugue static [PATH]
gitfugue history [PATH]

Shared flags:
  -o, --out FILE        output path; format by extension (.mid | .wav)
      --play            play after rendering
      --seed HEX        override seed (exploration/debugging)
      --scale NAME      pentatonic | minor-pentatonic | dorian | aeolian
      --bpm N           override base tempo
      --format mid|wav  default mid
      --verbose         print liner notes

Static:
      --duration SECS   target length (default 90-180 auto)

History:
      --range A..B      commit range
      --branches LIST   restrict voices
      --voices N        max simultaneous voices (default 6)
      --full            no commit cap (enables bar compression)
```

**Liner notes** (`--verbose`): a deterministic, human-readable decision log. Example:

```
key: A minor pentatonic  bpm: 84  palette: chamber
voice 0: main (anchor, cello)
alice@example.com -> piano (412 commits)
bob@example.com -> marimba (98 commits)
dependabot[bot] -> hi-hat (61 commits)
bar 47: merge feature/auth -> main, clean, plagal cadence
```

This doubles as the demo script and the debugging tool.

**Repo-committed config**: `.gitfugue.toml` at repo root can pin scale, palette, and BPM. Repos get to choose their own sound, and it ships with the code.

## 8. Tech stack

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust | Single static binary. Go acceptable; Rust wins on tree-sitter, midly, rustysynth maturity. |
| Git access | `gix` (gitoxide) | Behind an extraction trait. Shelling out to `git log --format=...` is an acceptable day-one shortcut inside that trait. |
| Parsing | tree-sitter | Compile in grammars for: Rust, Python, JS/TS, Go, C. Each grammar adds binary weight; start with 5 plus the indent fallback. |
| MIDI out | `midly` | |
| Synth | `rustysynth` (SF2) | Embed a small soundfont (2 to 8 MB class, e.g. TimGM6mb; verify license). `--soundfont` to swap. |
| Playback | `rodio` | |

## 9. Determinism requirements

- All iteration explicitly ordered. One seeded PRNG per scope, derived hierarchically (identity seed -> section seed -> unit seed).
- **Golden tests**: small fixture repos in `tests/fixtures/`, assert byte-identical `.mid` output.
- MIDI metadata carries engine version + seed. Bumping the engine version may change songs; that is documented and versioned, never silent.

## 10. Phases

**Phase 1 (first build session): static mode end-to-end.**
- Rust skeleton, extraction trait (shell-out git is fine day one)
- File walk, metrics (LOC, indent nesting), tree-sitter for 2 or 3 languages + indent fallback
- Composition engine v1: pentatonic, grid, contour templates, tension, drone harmony
- MIDI output, golden determinism test
- Exit criterion: run on a real repo, output stays in key and has audible phrase structure. Not a fax machine.

**Phase 2: history mode, single voice.**
- First-parent walk of trunk: theme + per-commit mutation
- Contributor instruments, bot percussion, tempo modulation, commit-range defaults
- `--verbose` liner notes

**Phase 3: the fugue.**
- Full DAG, lane allocation, fugal entries, merge cadences, voice cap + eviction
- Conflict detection via merge-tree

**Phase 4: sound and polish.**
- Embedded soundfont, WAV render, `--play`
- `.gitfugue.toml`
- Duration budgeting hardening, perf pass on large repos
- Stretch: `gitfugue watch` (re-render on commit), CI artifact mode ("hear this PR")

## 11. Open questions

1. **Squash-culture repos render thin.** Accept as honest signal, or add a hybrid mode: history as melody over static-mode-of-HEAD as harmonic accompaniment? (Hybrid is promising: melody = how it was built, harmony = what it is. Phase 4 experiment.)
2. **Rebase retcons the score.** Accept and document: a rewritten history is a different song. That is the point of the tool.
3. **Merge reconciliation algorithm.** How exactly to interleave two theme states so the result sounds like resolution rather than mush. Needs ear-testing, not spec-testing.
4. **Which SF2 to embed.** Size vs license vs sound quality.
5. **Nesting depth vs real complexity metrics.** Phase 1 uses the proxy; revisit only if the ear says complexity mapping feels wrong.
6. **Default mode.** Bare `gitfugue` currently specced as history mode; static might be the better zero-config first impression. Decide after hearing both.

## 12. Success criteria

- Under 5 seconds to MIDI on a 1k-file repo or 500-commit range.
- Byte-identical output for identical input, enforced by CI golden tests.
- The ear test, in order of difficulty:
  1. Two different repos sound recognizably different.
  2. A listener can hear a fork happen and a merge resolve.
  3. A listener can hear a second contributor join a project.
