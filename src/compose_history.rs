//! History-mode composer: the fugue (spec §6, Phase 3).
//!
//! Branch = voice, assigned by the lane allocator. The piece opens
//! with trunk stating the subject; a fork copies the parent's theme
//! state and enters transposed by a consonant interval; every commit
//! mutates its branch's theme (seeded by its own hash, magnitude by
//! diff size); a merge sounds both voices together for one bar, then
//! the target adopts a reconciled theme and the source releases.
//! Conflicted merges get a suspension bar first. While one voice
//! leads, the other active voices comp softly underneath, consonance
//! forced on strong beats. Bots play percussion.

use std::collections::BTreeMap;

use crate::compose::{ComposeParams, BAR, SLOT};
use crate::model::{
    Author, CommitNode, Key, NoteEvent, Scale, Score, Tick, Voice, LANE_ENSEMBLE,
};
use crate::rng::{fnv1a, Rng};
use crate::theme::{ops_for_diff, Theme};
use crate::theory::steps_to_midi;

/// Lead instruments for top contributors (spec §6.3): the author email
/// hash picks the slot, so the mapping is stable across renders and
/// even across repos.
const LEAD_PALETTE: [(u8, &str); 10] = [
    (0, "piano"),
    (12, "marimba"),
    (4, "electric piano"),
    (40, "violin"),
    (73, "flute"),
    (60, "french horn"),
    (11, "vibraphone"),
    (68, "oboe"),
    (24, "nylon guitar"),
    (46, "harp"),
];
const MAX_LEADS: usize = 4;
const ENSEMBLE: (u8, &str) = (48, "string ensemble");
const ANCHOR: (u8, &str) = (42, "cello");

/// GM percussion notes (channel 10).
const HH_CLOSED: u8 = 42;
const HH_OPEN: u8 = 46;

struct LaneVoice {
    theme: Theme,
    /// Bars played on this lane instance (indexes the theme cycle).
    bars_played: u32,
    active: bool,
    /// GM program currently sounding on this lane's channel.
    current_program: u8,
    name: String,
}

pub fn compose_history(
    identity_seed: u64,
    branch: &str,
    commits: &[CommitNode],
    authors: &[Author],
    total_commits: u32,
    params: &ComposeParams,
) -> Score {
    let root = Rng::new(identity_seed);
    let mut notes: Vec<NoteEvent> = Vec::new();
    let mut programs: Vec<(Tick, usize, u8)> = Vec::new();
    let mut liner = Vec::new();

    // ---- global identity: fixed at the repo's birth (spec §4.1) ----
    let mut grng = root.child("global");
    let key = Key {
        root: grng.below(12) as u8,
    };
    let scale = params.scale.unwrap_or_else(|| {
        if grng.weighted(&[60, 40]) == 0 {
            Scale::MajorPentatonic
        } else {
            Scale::MinorPentatonic
        }
    });
    let bpm = params.bpm.unwrap_or_else(|| grng.range(70, 110) as u16);
    let center_midi = (60 + grng.range(-3, 4)) as u8;
    let scale_len = scale.degrees().len() as i32;

    liner.push(format!(
        "key: {} {}  bpm: {}  (identity from root commit)",
        key.name(),
        scale.name(),
        bpm
    ));
    let folded: u32 = commits.iter().map(|c| c.folded).sum();
    let lane_count = commits
        .iter()
        .filter(|c| c.lane != LANE_ENSEMBLE)
        .map(|c| c.lane as usize + 1)
        .max()
        .unwrap_or(1);
    if commits.iter().any(|c| c.folded > 1) {
        liner.push(format!(
            "range: {} commits on {} compressed into {} bars (of {} total)",
            folded,
            branch,
            commits.len(),
            total_commits
        ));
    } else {
        liner.push(format!(
            "range: {} commits, {} voices used (of {} total commits)",
            commits.len(),
            lane_count,
            total_commits
        ));
    }
    liner.push(format!("voice 0: {} (anchor, {})", branch, ANCHOR.1));

    // ---- instruments ----
    let (voices, program_of_author, ensemble_voice, bot_voice) =
        assign_instruments(authors, lane_count, &mut liner);

    // ---- lanes ----
    let subject = Theme::generate(&mut root.child("subject"), key, scale, center_midi);
    liner.push(format!("subject: {} bars, stated by {}", subject.bars, branch));
    let mut lanes: Vec<LaneVoice> = (0..lane_count)
        .map(|l| LaneVoice {
            theme: subject.clone(),
            bars_played: 0,
            active: l == 0,
            current_program: if l == 0 { ANCHOR.0 } else { ENSEMBLE.0 },
            name: if l == 0 { branch.to_string() } else { String::new() },
        })
        .collect();
    let mut ensemble_theme = subject.clone();
    let mut ensemble_active = false;

    // ---- tempo tiers from timestamp deltas (spec §6.1) ----
    let deltas: Vec<i64> = commits
        .windows(2)
        .map(|w| (w[1].timestamp - w[0].timestamp).max(0))
        .collect();
    let median = median_positive(&deltas);
    const BREATH_FLOOR: i64 = 3 * 86_400;

    let mut tempo_map: Vec<(Tick, u16)> = vec![(0, bpm)];
    let mut tick: Tick = 0;

    // ---- exposition: trunk states the subject (spec §6.2) ----
    for bar in 0..subject.bars {
        play_bar(
            &mut notes,
            &subject,
            bar,
            tick,
            0,
            key,
            scale,
            64, // moderate, unmodulated statement
            &mut root.child("expo"),
        );
        tick += BAR;
    }
    lanes[0].bars_played = subject.bars;

    // ---- one commit = one bar ----
    for (i, commit) in commits.iter().enumerate() {
        // Breath rest on real hiatuses, capped at a single bar.
        if i > 0 && median > 0 && deltas[i - 1] > (8 * median).max(BREATH_FLOOR) {
            liner.push(format!(
                "bar {}: {}-day gap, breath",
                bar_no(tick),
                deltas[i - 1] / 86_400
            ));
            notes.push(NoteEvent {
                voice: 0,
                pitch: steps_to_midi(
                    key,
                    scale,
                    crate::theory::midi_to_steps(key, scale, center_midi) - scale_len,
                ),
                start: tick,
                dur: BAR,
                velocity: 30,
            });
            tick += BAR;
        }

        let local = local_bpm(bpm, deltas.get(i.wrapping_sub(1)).copied(), median);
        if local != tempo_map.last().unwrap().1 {
            tempo_map.push((tick, local));
        }

        let author = &authors[commit.author_id];
        let is_bot = author.is_bot;
        let on_ensemble = commit.lane == LANE_ENSEMBLE;
        let lane = if on_ensemble { 0 } else { commit.lane as usize };
        let voice = if on_ensemble { ensemble_voice.unwrap_or(0) } else { lane };

        // Fork: the new voice copies the parent's theme state and
        // enters with it transposed by a 4th or 5th, chosen by the
        // branch-name hash (spec §6.2).
        if commit.opens_lane && !on_ensemble {
            if let Some(evicted) = commit.evicted_lane {
                liner.push(format!(
                    "bar {}: voice {} folds into ensemble (least recently active)",
                    bar_no(tick),
                    evicted
                ));
                ensemble_active = true;
                ensemble_theme = lanes[evicted as usize].theme.clone();
            }
            let label = commit
                .fork_name
                .clone()
                .unwrap_or_else(|| format!("branch-{}", commit.short));
            let interval = if fnv1a(label.as_bytes()) & 1 == 0 {
                (scale_len * 3) / 5 // up a 5th (3 steps pentatonic, 4 diatonic)
            } else {
                -(scale_len * 2) / 5 // down a 4th-ish
            };
            let parent_lane = commit
                .parents
                .first()
                .map(|&p| {
                    let pl = commits[p].lane;
                    if pl == LANE_ENSEMBLE { 0 } else { pl as usize }
                })
                .unwrap_or(0);
            let mut theme = lanes[parent_lane].theme.clone();
            theme.transpose(interval);
            let prev_program = lanes[lane].current_program;
            lanes[lane] = LaneVoice {
                theme,
                bars_played: 0,
                active: true,
                current_program: prev_program,
                name: label.clone(),
            };
            liner.push(format!(
                "bar {}: voice {} enters ({}, {})",
                bar_no(tick),
                lane,
                label,
                if interval > 0 { "up a 5th" } else { "down a 4th" }
            ));
        }

        // The commit mutates its own branch's theme before the bar
        // sounds; musical distance tracks code distance (spec §6.2).
        let mut crng = Rng::new(commit.hash).child("mutation");
        let ops = ops_for_diff(commit.diff_magnitude);
        if on_ensemble {
            ensemble_active = true;
            ensemble_theme.mutate(&mut crng, ops);
        } else {
            lanes[lane].theme.mutate(&mut crng, ops);
        }

        // Timbre: each bar speaks in its commit author's instrument.
        if !is_bot && !on_ensemble {
            let program = program_of_author[&commit.author_id];
            if program != lanes[lane].current_program {
                programs.push((tick, voice, program));
                lanes[lane].current_program = program;
            }
        }

        // Conflicted merge: one bar of suspension first (spec §6.2).
        if commit.is_merge && commit.conflicted && commit.closes_lane.is_some() {
            let src = commit.closes_lane.unwrap() as usize;
            liner.push(format!("bar {}: conflict tension", bar_no(tick)));
            suspension_bar(&mut notes, &lanes, lane, src, tick, key, scale);
            tick += BAR;
        }

        let mut brng = Rng::new(commit.hash).child("bar");
        if is_bot {
            if let Some(v) = bot_voice {
                play_percussion_bar(&mut notes, tick, v, commit.diff_magnitude, &mut brng);
            }
        } else {
            let theme_ref = if on_ensemble { &ensemble_theme } else { &lanes[lane].theme };
            let cycle_bar = if on_ensemble {
                (i as u32) % theme_ref.bars
            } else {
                lanes[lane].bars_played % theme_ref.bars
            };
            let vel = 68 + ((commit.diff_magnitude as u64 + 1).ilog2().min(8) as u8);
            play_bar(
                &mut notes, theme_ref, cycle_bar, tick, voice, key, scale, vel, &mut brng,
            );
            if !on_ensemble {
                lanes[lane].bars_played += 1;
            }
        }

        // Merge cadence: both voices sound together for this bar, then
        // the target adopts the reconciled theme and the source
        // releases (spec §6.2).
        if let Some(src) = commit.closes_lane {
            let src = src as usize;
            if lanes[src].active {
                let src_bar = lanes[src].bars_played % lanes[src].theme.bars;
                let mut srng = Rng::new(commit.hash).child("cadence");
                play_bar(
                    &mut notes,
                    &lanes[src].theme,
                    src_bar,
                    tick,
                    src,
                    key,
                    scale,
                    60,
                    &mut srng,
                );
                let mut mrng = Rng::new(commit.hash).child("reconcile");
                let merged = Theme::reconcile(
                    if on_ensemble { &ensemble_theme } else { &lanes[lane].theme },
                    &lanes[src].theme,
                    &mut mrng,
                );
                if on_ensemble {
                    ensemble_theme = merged;
                } else {
                    lanes[lane].theme = merged;
                }
                let released = std::mem::take(&mut lanes[src].name);
                lanes[src].active = false;
                liner.push(format!(
                    "bar {}: merge {} -> {}, {}, cadence",
                    bar_no(tick),
                    if released.is_empty() { commit.short.clone() } else { released },
                    if lane == 0 { branch.to_string() } else { format!("voice {lane}") },
                    if commit.conflicted { "conflicted" } else { "clean" }
                ));
            }
            // Cadence marker: low tonic under the resolution.
            notes.push(NoteEvent {
                voice: 0,
                pitch: 36 + key.root,
                start: tick,
                dur: BAR / 2,
                velocity: 58,
            });
        }

        // Comping: every other active voice holds its theme's strong
        // note softly, consonant on the downbeat (spec §4 harmony).
        let lead_slot0 = lead_downbeat(&notes, tick, voice);
        for (l, lv) in lanes.iter().enumerate() {
            if !lv.active || l == lane || (on_ensemble && l == 0 && voice == l) {
                continue;
            }
            if Some(l as u8) == commit.closes_lane {
                continue; // already sounding its full cadence line
            }
            comp_bar(&mut notes, lv, l, tick, key, scale, lead_slot0);
        }
        if ensemble_active && !on_ensemble && ensemble_voice.is_some() {
            let lv = LaneVoice {
                theme: ensemble_theme.clone(),
                    bars_played: 0,
                active: true,
                current_program: ENSEMBLE.0,
                name: String::new(),
            };
            comp_bar(&mut notes, &lv, ensemble_voice.unwrap(), tick, key, scale, lead_slot0);
        }

        tick += BAR;
    }

    // Outro: trunk restates the subject's first bar and settles.
    programs.push((tick, 0, ANCHOR.0));
    let mut orng = root.child("outro");
    play_bar(&mut notes, &subject, 0, tick, 0, key, scale, 56, &mut orng);
    tick += BAR;
    notes.push(NoteEvent {
        voice: 0,
        pitch: steps_to_midi(key, scale, crate::theory::midi_to_steps(key, scale, center_midi)),
        start: tick,
        dur: BAR,
        velocity: 54,
    });
    notes.push(NoteEvent {
        voice: 0,
        pitch: 36 + key.root,
        start: tick,
        dur: BAR,
        velocity: 46,
    });

    notes.sort_by_key(|e| (e.start, e.voice, e.pitch));
    programs.sort_by_key(|e| (e.0, e.1));

    Score {
        bpm_base: bpm,
        key,
        scale,
        voices,
        events: notes,
        tempo_map,
        program_changes: programs,
        liner_notes: liner,
        seed: identity_seed,
    }
}

/// Voice layout: one voice per lane (lane 0 = trunk/anchor), then the
/// shared ensemble, then percussion. Authors map to GM programs that
/// get switched onto lane channels per bar.
#[allow(clippy::type_complexity)]
fn assign_instruments(
    authors: &[Author],
    lane_count: usize,
    liner: &mut Vec<String>,
) -> (Vec<Voice>, BTreeMap<usize, u8>, Option<usize>, Option<usize>) {
    let mut voices = Vec::new();
    let mut next_channel = 0u8;
    let mut alloc_channel = |percussion: bool| -> u8 {
        if percussion {
            return 9;
        }
        if next_channel == 9 {
            next_channel = 10;
        }
        let c = next_channel;
        next_channel += 1;
        c
    };

    for l in 0..lane_count {
        voices.push(Voice {
            name: if l == 0 {
                format!("trunk ({})", ANCHOR.1)
            } else {
                format!("lane {l}")
            },
            channel: alloc_channel(false),
            program: if l == 0 { ANCHOR.0 } else { ENSEMBLE.0 },
        });
    }

    // Rank human authors for lead instruments (spec §6.3).
    let mut ranked: Vec<usize> = (0..authors.len()).filter(|i| !authors[*i].is_bot).collect();
    ranked.sort_by(|a, b| {
        authors[*b]
            .commits
            .cmp(&authors[*a].commits)
            .then(authors[*a].email.cmp(&authors[*b].email))
    });
    let mut program_of: BTreeMap<usize, u8> = BTreeMap::new();
    let mut taken = [false; LEAD_PALETTE.len()];
    for &ai in ranked.iter().take(MAX_LEADS) {
        let mut slot = (fnv1a(authors[ai].email.as_bytes()) % LEAD_PALETTE.len() as u64) as usize;
        while taken[slot] {
            slot = (slot + 1) % LEAD_PALETTE.len();
        }
        taken[slot] = true;
        let (program, name) = LEAD_PALETTE[slot];
        liner.push(format!(
            "{} -> {} ({} commits)",
            authors[ai].email, name, authors[ai].commits
        ));
        program_of.insert(ai, program);
    }
    let tail: Vec<usize> = ranked.iter().skip(MAX_LEADS).copied().collect();
    if !tail.is_empty() {
        let commits: u32 = tail.iter().map(|&i| authors[i].commits).sum();
        liner.push(format!(
            "{} more authors -> {} ({} commits)",
            tail.len(),
            ENSEMBLE.1,
            commits
        ));
        for ai in tail {
            program_of.insert(ai, ENSEMBLE.0);
        }
    }

    // The shared ensemble voice exists whenever overflow can happen;
    // allocate it lazily only if some author needs it or lanes overflow.
    let ensemble_voice = {
        let v = voices.len();
        voices.push(Voice {
            name: "ensemble".to_string(),
            channel: alloc_channel(false),
            program: ENSEMBLE.0,
        });
        Some(v)
    };

    let bots: Vec<usize> = (0..authors.len()).filter(|i| authors[*i].is_bot).collect();
    let mut bot_voice = None;
    if !bots.is_empty() {
        for &ai in &bots {
            liner.push(format!(
                "{} -> hi-hat ({} commits)",
                authors[ai].name, authors[ai].commits
            ));
        }
        let v = voices.len();
        voices.push(Voice {
            name: "hi-hat".to_string(),
            channel: alloc_channel(true),
            program: 0,
        });
        bot_voice = Some(v);
    }

    (voices, program_of, ensemble_voice, bot_voice)
}

/// Render one bar of a theme. `vel_base` centers the dynamics.
#[allow(clippy::too_many_arguments)]
fn play_bar(
    notes: &mut Vec<NoteEvent>,
    theme: &Theme,
    bar: u32,
    start: Tick,
    voice: usize,
    key: Key,
    scale: Scale,
    vel_base: u8,
    rng: &mut Rng,
) {
    let bar_notes = theme.bar(bar);
    for (i, &(slot, degree, weight)) in bar_notes.iter().enumerate() {
        let gap = if i + 1 < bar_notes.len() {
            bar_notes[i + 1].0 - slot
        } else {
            16 - slot
        };
        let mut vel = vel_base as f64 + weight as f64 * 6.0;
        vel += rng.f64() * 12.0 - 6.0;
        notes.push(NoteEvent {
            voice,
            pitch: steps_to_midi(key, scale, degree),
            start: start + slot * SLOT,
            dur: gap.clamp(1, 4) * SLOT,
            velocity: vel.clamp(30.0, 115.0) as u8,
        });
    }
}

/// The lead voice's pitch on the downbeat of the bar starting at `tick`.
fn lead_downbeat(notes: &[NoteEvent], tick: Tick, voice: usize) -> Option<u8> {
    notes
        .iter()
        .rev()
        .take_while(|e| e.start >= tick)
        .filter(|e| e.voice == voice && e.start == tick)
        .map(|e| e.pitch)
        .next()
}

/// A soft held note from a non-lead voice: polyphony without mush.
/// On the downbeat the interval against the lead is forced consonant.
fn comp_bar(
    notes: &mut Vec<NoteEvent>,
    lv: &LaneVoice,
    voice: usize,
    tick: Tick,
    key: Key,
    scale: Scale,
    lead_downbeat: Option<u8>,
) {
    let bar = lv.bars_played % lv.theme.bars.max(1);
    let strongest = lv.theme.bar(bar).into_iter().max_by_key(|n| (n.2, std::cmp::Reverse(n.0)));
    let (_, degree, _) = match strongest {
        Some(n) => n,
        None => return,
    };
    let mut degree = degree;
    if let Some(lead) = lead_downbeat {
        // Semitone or major-7th or tritone against the lead: step away.
        let iv = (steps_to_midi(key, scale, degree) as i32 - lead as i32).rem_euclid(12);
        if matches!(iv, 1 | 6 | 11) {
            degree -= 1;
        }
    }
    notes.push(NoteEvent {
        voice,
        pitch: steps_to_midi(key, scale, degree),
        start: tick,
        dur: BAR,
        velocity: 42,
    });
}

/// One bar of held tension before a conflicted merge resolves: the two
/// voices a step apart, leaning until the cadence (spec §6.2).
fn suspension_bar(
    notes: &mut Vec<NoteEvent>,
    lanes: &[LaneVoice],
    target: usize,
    source: usize,
    tick: Tick,
    key: Key,
    scale: Scale,
) {
    let t_deg = lanes[target]
        .theme
        .bar(0)
        .first()
        .map(|n| n.1)
        .unwrap_or(0);
    // A second apart: maximal in-scale tension, resolving down at
    // beat 3 into a third.
    for (voice, deg, resolve) in [(target, t_deg, t_deg), (source, t_deg + 1, t_deg - 1)] {
        notes.push(NoteEvent {
            voice,
            pitch: steps_to_midi(key, scale, deg),
            start: tick,
            dur: BAR / 2,
            velocity: 70,
        });
        notes.push(NoteEvent {
            voice,
            pitch: steps_to_midi(key, scale, resolve),
            start: tick + BAR / 2,
            dur: BAR / 2,
            velocity: 62,
        });
    }
}

/// A bot bar: hi-hat eighths, with sixteenths creeping in as the diff
/// grows, and an occasional open hat at the turnaround.
fn play_percussion_bar(
    notes: &mut Vec<NoteEvent>,
    start: Tick,
    voice: usize,
    diff_magnitude: u32,
    rng: &mut Rng,
) {
    let m = (diff_magnitude as u64 + 1).ilog2();
    let step = if m >= 6 { 1 } else { 2 }; // 16ths for big bumps, 8ths otherwise
    let mut slot = 0;
    while slot < 16 {
        let open = slot == 14 && rng.f64() < 0.3;
        let vel = if slot % 4 == 0 { 72 } else { 56 } + (rng.below(9) as u8);
        notes.push(NoteEvent {
            voice,
            pitch: if open { HH_OPEN } else { HH_CLOSED },
            start: start + slot * SLOT,
            dur: SLOT,
            velocity: vel,
        });
        slot += step;
    }
}

fn local_bpm(base: u16, delta: Option<i64>, median: i64) -> u16 {
    let (delta, median) = match (delta, median) {
        (Some(d), m) if m > 0 => (d, m),
        _ => return base,
    };
    // Stepped tiers keep this integer-exact and byte-reproducible.
    let ratio_pct = (delta.max(0) as u128 * 100 / median as u128) as u64;
    let pct: u32 = match ratio_pct {
        0..=49 => 115,
        50..=79 => 108,
        80..=125 => 100,
        126..=400 => 92,
        _ => 85,
    };
    ((base as u32 * pct / 100).max(30)) as u16
}

fn median_positive(deltas: &[i64]) -> i64 {
    let mut pos: Vec<i64> = deltas.iter().copied().filter(|d| *d > 0).collect();
    if pos.is_empty() {
        return 0;
    }
    pos.sort_unstable();
    pos[pos.len() / 2]
}

fn bar_no(tick: Tick) -> u32 {
    tick / BAR + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authors() -> Vec<Author> {
        vec![
            Author { email: "alice@example.com".into(), name: "Alice".into(), commits: 5, is_bot: false },
            Author { email: "bob@example.com".into(), name: "Bob".into(), commits: 3, is_bot: false },
            Author { email: "49699333+dependabot[bot]@users.noreply.github.com".into(), name: "dependabot[bot]".into(), commits: 2, is_bot: true },
        ]
    }

    fn node(i: u64, author: usize, parents: Vec<usize>, lane: u8) -> CommitNode {
        CommitNode {
            hash: 0x1000 + i * 0x9E37_79B9,
            short: format!("{i:07x}"),
            parents,
            author_id: author,
            timestamp: 1_600_000_000 + i as i64 * 86_400,
            diff_magnitude: (i as u32 + 1) * 37,
            is_merge: false,
            folded: 1,
            lane,
            opens_lane: false,
            closes_lane: None,
            fork_name: None,
            conflicted: false,
            evicted_lane: None,
        }
    }

    /// trunk: 0-1-2----5(merge)-6   branch: 3-4 (forked from 1)
    fn dag() -> Vec<CommitNode> {
        let mut c = vec![
            node(0, 0, vec![], 0),
            node(1, 0, vec![0], 0),
            node(2, 1, vec![1], 0),
            node(3, 1, vec![1], 1),
            node(4, 2, vec![3], 1),
            node(5, 0, vec![2, 4], 0),
            node(6, 0, vec![5], 0),
        ];
        c[3].opens_lane = true;
        c[3].fork_name = Some("feature/x".into());
        c[5].is_merge = true;
        c[5].closes_lane = Some(1);
        c
    }

    fn params() -> ComposeParams {
        ComposeParams { scale: None, bpm: None, duration_secs: None }
    }

    #[test]
    fn deterministic() {
        let a = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        let b = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        assert_eq!(a.events.len(), b.events.len());
        for (x, y) in a.events.iter().zip(&b.events) {
            assert_eq!(
                (x.voice, x.pitch, x.start, x.dur, x.velocity),
                (y.voice, y.pitch, y.start, y.dur, y.velocity)
            );
        }
        assert_eq!(a.program_changes, b.program_changes);
    }

    #[test]
    fn fork_enters_on_its_own_voice() {
        let s = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        assert!(s.voices.len() >= 2);
        let lane1_notes: Vec<_> = s.events.iter().filter(|e| e.voice == 1).collect();
        assert!(!lane1_notes.is_empty(), "branch voice must sound");
        assert!(s.liner_notes.iter().any(|l| l.contains("enters (feature/x")));
    }

    #[test]
    fn merge_bar_sounds_both_voices() {
        let s = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        // Find the merge bar from the liner, then check both voices
        // have onsets inside it.
        let merge_line = s
            .liner_notes
            .iter()
            .find(|l| l.contains("merge feature/x"))
            .expect("merge liner entry");
        let bar: u32 = merge_line
            .split(&[' ', ':'][..])
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let start = (bar - 1) * BAR;
        let in_bar = |v: usize| {
            s.events
                .iter()
                .any(|e| e.voice == v && e.start >= start && e.start < start + BAR)
        };
        assert!(in_bar(0) && in_bar(1), "cadence must overlay both voices");
        // After the merge the source voice releases (only comping/lead
        // events before, nothing after the cadence bar).
        let after = start + BAR;
        let lane1_after = s
            .events
            .iter()
            .filter(|e| e.voice == 1 && e.start >= after)
            .count();
        assert_eq!(lane1_after, 0, "released voice must fall silent");
    }

    #[test]
    fn voices_overlap_while_branch_active() {
        let s = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        // During branch bars (commits 3,4) trunk comps underneath:
        // some tick must have simultaneous notes from voices 0 and 1.
        let mut overlap = false;
        for e0 in s.events.iter().filter(|e| e.voice == 0) {
            if s.events.iter().any(|e1| {
                e1.voice == 1 && e1.start < e0.start + e0.dur && e0.start < e1.start + e1.dur
            }) {
                overlap = true;
                break;
            }
        }
        assert!(overlap, "fugue must actually overlay voices");
    }

    #[test]
    fn conflicted_merge_adds_suspension() {
        let mut d = dag();
        d[5].conflicted = true;
        let clean = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        let tense = compose_history(0xFEED, "main", &d, &authors(), 7, &params());
        assert!(tense.liner_notes.iter().any(|l| l.contains("conflict tension")));
        let end = |s: &Score| s.events.iter().map(|e| e.start + e.dur).max().unwrap();
        assert_eq!(end(&tense) - end(&clean), BAR, "suspension inserts one bar");
    }

    #[test]
    fn author_timbres_switch_on_one_lane() {
        let s = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        // Trunk hosts alice and bob commits: at least one program
        // change on voice 0 beyond the outro's anchor reset.
        let trunk_changes: Vec<_> = s
            .program_changes
            .iter()
            .filter(|(_, v, _)| *v == 0)
            .collect();
        assert!(
            trunk_changes.len() >= 2,
            "expected author timbre changes on trunk, got {trunk_changes:?}"
        );
    }

    #[test]
    fn bots_play_percussion() {
        let s = compose_history(0xFEED, "main", &dag(), &authors(), 7, &params());
        let hh = s.voices.iter().position(|v| v.name == "hi-hat").unwrap();
        assert_eq!(s.voices[hh].channel, 9);
        let bot_notes: Vec<_> = s.events.iter().filter(|e| e.voice == hh).collect();
        assert!(!bot_notes.is_empty());
        assert!(bot_notes.iter().all(|e| e.pitch == HH_CLOSED || e.pitch == HH_OPEN));
    }

    #[test]
    fn melody_stays_in_scale() {
        let p = ComposeParams { scale: Some(Scale::Aeolian), bpm: Some(90), duration_secs: None };
        let s = compose_history(0xFEED, "main", &dag(), &authors(), 7, &p);
        let hh = s.voices.iter().position(|v| v.name == "hi-hat").unwrap();
        for e in s.events.iter().filter(|e| e.voice != hh) {
            let pc = ((e.pitch as i32 - s.key.root as i32).rem_euclid(12)) as u8;
            assert!(
                Scale::Aeolian.degrees().contains(&pc),
                "pitch {} (pc {pc}) off-scale",
                e.pitch
            );
        }
    }
}
