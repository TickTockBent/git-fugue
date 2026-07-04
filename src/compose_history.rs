//! History-mode composer (spec §6, Phase 2: single voice).
//!
//! First-parent walk of trunk: the piece opens with an exposition of
//! the subject, then one commit = one bar. Each commit mutates the
//! theme (seeded by its own hash, magnitude by diff size) and plays the
//! current theme state in its author's timbre. Bots play percussion.
//! Timestamp deltas modulate local tempo +/-15% and long gaps insert a
//! capped breath bar.

use std::collections::BTreeMap;

use crate::compose::{ComposeParams, BAR, SLOT};
use crate::model::{
    Author, CommitNode, Key, NoteEvent, Scale, Score, Tick, Voice,
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

#[allow(clippy::too_many_arguments)]
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

    liner.push(format!(
        "key: {} {}  bpm: {}  (identity from root commit)",
        key.name(),
        scale.name(),
        bpm
    ));
    let folded: u32 = commits.iter().map(|c| c.folded).sum();
    if commits.iter().any(|c| c.folded > 1) {
        liner.push(format!(
            "range: {} first-parent commits on {} compressed into {} bars (of {} total)",
            folded,
            branch,
            commits.len(),
            total_commits
        ));
    } else {
        liner.push(format!(
            "range: {} first-parent commits on {} (of {} total)",
            commits.len(),
            branch,
            total_commits
        ));
    }

    // ---- voices: anchor + top-N author leads + tail + percussion ----
    let (voices, voice_of_author, bot_voice) = assign_voices(authors, &mut liner);

    // ---- the subject ----
    let mut theme = Theme::generate(&mut root.child("subject"), key, scale, center_midi);
    liner.push(format!("subject: {} bars, stated by anchor", theme.bars));

    // ---- tempo tiers from timestamp deltas (spec §6.1) ----
    let deltas: Vec<i64> = commits
        .windows(2)
        .map(|w| (w[1].timestamp - w[0].timestamp).max(0))
        .collect();
    let median = median_positive(&deltas);

    let mut tempo_map: Vec<(Tick, u16)> = vec![(0, bpm)];
    let mut tick: Tick = 0;

    // Exposition: the trunk states the subject verbatim.
    for bar in 0..theme.bars {
        play_theme_bar(&mut notes, &theme, bar, tick, 0, key, scale, 0, &mut root.child("expo"));
        tick += BAR;
    }

    // A breath needs a real hiatus: well beyond the repo's own rhythm
    // AND multiple days of wall clock. Burst-committed repos have a
    // tiny median delta; without the floor every coffee break breathes.
    const BREATH_FLOOR: i64 = 3 * 86_400;

    // One commit = one bar.
    for (i, commit) in commits.iter().enumerate() {
        // Breath rest on long gaps, capped at a single bar (spec §6.1:
        // never literal silence proportional to wall-clock time).
        if i > 0 && median > 0 && deltas[i - 1] > (8 * median).max(BREATH_FLOOR) {
            let gap = deltas[i - 1];
            liner.push(format!(
                "bar {}: {}-day gap, breath",
                bar_no(tick),
                gap / 86_400
            ));
            notes.push(NoteEvent {
                voice: 0,
                pitch: steps_to_midi(key, scale, crate::theory::midi_to_steps(key, scale, center_midi) - 7),
                start: tick,
                dur: BAR,
                velocity: 30,
            });
            tick += BAR;
        }

        // Local tempo from commit cadence.
        let local = local_bpm(bpm, deltas.get(i.wrapping_sub(1)).copied(), median);
        if local != tempo_map.last().unwrap().1 {
            tempo_map.push((tick, local));
        }

        // The commit mutates the theme before its bar sounds.
        let mut crng = Rng::new(commit.hash).child("mutation");
        theme.mutate(&mut crng, ops_for_diff(commit.diff_magnitude));

        let author = &authors[commit.author_id];
        let mut brng = Rng::new(commit.hash).child("bar");
        if author.is_bot {
            let v = bot_voice.expect("bot commit implies bot voice");
            play_percussion_bar(&mut notes, tick, v, commit.diff_magnitude, &mut brng);
        } else {
            let voice = voice_of_author[&commit.author_id];
            let bar = (i as u32) % theme.bars;
            play_theme_bar(
                &mut notes,
                &theme,
                bar,
                tick,
                voice,
                key,
                scale,
                commit.diff_magnitude,
                &mut brng,
            );
        }
        if commit.is_merge {
            // First-parent view of a merge: a low tonic strike marks
            // the cadence point (full merge treatment is Phase 3).
            liner.push(format!("bar {}: merge {}", bar_no(tick), commit.short));
            notes.push(NoteEvent {
                voice: 0,
                pitch: 36 + key.root,
                start: tick,
                dur: BAR / 2,
                velocity: 58,
            });
        }
        tick += BAR;
    }

    // Outro: the anchor settles on the tonic.
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
    tick += BAR;
    let _ = tick;

    notes.sort_by_key(|e| (e.start, e.voice, e.pitch));

    Score {
        bpm_base: bpm,
        key,
        scale,
        voices,
        events: notes,
        tempo_map,
        liner_notes: liner,
        seed: identity_seed,
    }
}

/// Anchor is always voice 0 (spec §6.2: trunk holds the anchor
/// instrument). Top-N human authors get lead instruments chosen by
/// email hash; the long tail shares the ensemble; bots share percussion.
fn assign_voices(
    authors: &[Author],
    liner: &mut Vec<String>,
) -> (Vec<Voice>, BTreeMap<usize, usize>, Option<usize>) {
    let mut voices = vec![Voice {
        name: format!("anchor ({})", ANCHOR.1),
        channel: 0,
        program: ANCHOR.0,
    }];
    let mut next_channel = 1u8;
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

    // Rank human authors by commit count (desc), then email, for the
    // lead slots. Order of *ranking* decides who gets a lead; the
    // instrument itself comes from the email hash.
    let mut ranked: Vec<usize> = (0..authors.len()).filter(|i| !authors[*i].is_bot).collect();
    ranked.sort_by(|a, b| {
        authors[*b]
            .commits
            .cmp(&authors[*a].commits)
            .then(authors[*a].email.cmp(&authors[*b].email))
    });

    let mut voice_of: BTreeMap<usize, usize> = BTreeMap::new();
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
        voice_of.insert(ai, voices.len());
        voices.push(Voice {
            name: name.to_string(),
            channel: alloc_channel(false),
            program,
        });
    }

    // Long tail shares one section instrument.
    let tail: Vec<usize> = ranked.iter().skip(MAX_LEADS).copied().collect();
    if !tail.is_empty() {
        let commits: u32 = tail.iter().map(|&i| authors[i].commits).sum();
        liner.push(format!(
            "{} more authors -> {} ({} commits)",
            tail.len(),
            ENSEMBLE.1,
            commits
        ));
        let v = voices.len();
        voices.push(Voice {
            name: ENSEMBLE.1.to_string(),
            channel: alloc_channel(false),
            program: ENSEMBLE.0,
        });
        for ai in tail {
            voice_of.insert(ai, v);
        }
    }

    // Bots: the percussion channel (spec §6.3). Dependabot is the hi-hat.
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
        for ai in bots {
            voice_of.insert(ai, v);
        }
        bot_voice = Some(v);
    }

    (voices, voice_of, bot_voice)
}

/// Render one bar of the current theme state. Diff magnitude modulates
/// density (spec §6.1): quiet commits thin to the strong notes, big
/// ones add echo ornaments.
#[allow(clippy::too_many_arguments)]
fn play_theme_bar(
    notes: &mut Vec<NoteEvent>,
    theme: &Theme,
    bar: u32,
    start: Tick,
    voice: usize,
    key: Key,
    scale: Scale,
    diff_magnitude: u32,
    rng: &mut Rng,
) {
    let mut bar_notes = theme.bar(bar);
    if bar_notes.is_empty() {
        return;
    }
    let m = (diff_magnitude as u64 + 1).ilog2();
    // Thin quiet bars: drop the lightest notes, keep at least 2.
    let drop = match m {
        0 => bar_notes.len().saturating_sub(2).min(2),
        1..=2 => 1,
        _ => 0,
    };
    for _ in 0..drop {
        if bar_notes.len() <= 2 {
            break;
        }
        let (i, _) = bar_notes
            .iter()
            .enumerate()
            .min_by_key(|(i, n)| (n.2, std::cmp::Reverse(*i)))
            .unwrap();
        bar_notes.remove(i);
    }
    // Big diffs earn echo ornaments after the strongest notes.
    let extra = match m {
        6..=8 => 1,
        _ if m > 8 => 2,
        _ => 0,
    };
    for _ in 0..extra {
        let strongest = bar_notes.iter().max_by_key(|n| (n.2, n.0)).copied();
        if let Some((slot, degree, _)) = strongest {
            let echo = slot + 1;
            if echo < 16 && !bar_notes.iter().any(|n| n.0 == echo) {
                bar_notes.push((echo, degree + if rng.weighted(&[50, 50]) == 0 { 1 } else { -1 }, 0));
                bar_notes.sort_by_key(|n| n.0);
            }
        }
    }

    for (i, &(slot, degree, weight)) in bar_notes.iter().enumerate() {
        let gap = if i + 1 < bar_notes.len() {
            bar_notes[i + 1].0 - slot
        } else {
            16 - slot
        };
        let mut vel = 68.0 + weight as f64 * 6.0 + (m.min(8)) as f64 * 1.5;
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

    fn commits() -> Vec<CommitNode> {
        let day = 86_400i64;
        (0..10u64)
            .map(|i| CommitNode {
                hash: 0x1000 + i * 0x9E3779B9,
                short: format!("{:07x}", i),
                author_id: match i % 4 {
                    0 | 1 => 0,
                    2 => 1,
                    _ => 2,
                },
                timestamp: 1_600_000_000 + i as i64 * day,
                diff_magnitude: (i as u32 + 1) * 37,
                is_merge: i == 7,
                folded: 1,
            })
            .collect()
    }

    fn params() -> ComposeParams {
        ComposeParams { scale: None, bpm: None, duration_secs: None }
    }

    #[test]
    fn deterministic() {
        let a = compose_history(0xFEED, "main", &commits(), &authors(), 10, &params());
        let b = compose_history(0xFEED, "main", &commits(), &authors(), 10, &params());
        assert_eq!(a.events.len(), b.events.len());
        for (x, y) in a.events.iter().zip(&b.events) {
            assert_eq!(
                (x.voice, x.pitch, x.start, x.dur, x.velocity),
                (y.voice, y.pitch, y.start, y.dur, y.velocity)
            );
        }
        assert_eq!(a.tempo_map, b.tempo_map);
    }

    #[test]
    fn identity_seed_sets_globals_commits_extend() {
        let short = compose_history(0xFEED, "main", &commits()[..5], &authors(), 5, &params());
        let long = compose_history(0xFEED, "main", &commits(), &authors(), 10, &params());
        // Same identity: same key, scale, tempo (spec §4.1).
        assert_eq!(short.key.root, long.key.root);
        assert_eq!(short.bpm_base, long.bpm_base);
        // More commits: longer piece.
        let end = |s: &Score| s.events.iter().map(|e| e.start + e.dur).max().unwrap();
        assert!(end(&long) > end(&short));
    }

    #[test]
    fn bots_play_percussion() {
        let s = compose_history(0xFEED, "main", &commits(), &authors(), 10, &params());
        let hh = s.voices.iter().position(|v| v.name == "hi-hat").unwrap();
        assert_eq!(s.voices[hh].channel, 9, "percussion must be channel 10");
        let bot_notes: Vec<_> = s.events.iter().filter(|e| e.voice == hh).collect();
        assert!(!bot_notes.is_empty(), "dependabot bars must drum");
        assert!(bot_notes.iter().all(|e| e.pitch == HH_CLOSED || e.pitch == HH_OPEN));
    }

    #[test]
    fn anchor_is_voice_zero() {
        let s = compose_history(0xFEED, "main", &commits(), &authors(), 10, &params());
        assert!(s.voices[0].name.contains("anchor"));
        // Exposition: the first notes belong to the anchor.
        assert_eq!(s.events.iter().min_by_key(|e| e.start).unwrap().voice, 0);
    }

    #[test]
    fn melody_stays_in_scale() {
        let p = ComposeParams { scale: Some(Scale::Dorian), bpm: Some(90), duration_secs: None };
        let s = compose_history(0xFEED, "main", &commits(), &authors(), 10, &p);
        let hh = s.voices.iter().position(|v| v.name == "hi-hat").unwrap();
        for e in s.events.iter().filter(|e| e.voice != hh) {
            let pc = ((e.pitch as i32 - s.key.root as i32).rem_euclid(12)) as u8;
            assert!(
                Scale::Dorian.degrees().contains(&pc),
                "pitch {} (pc {pc}) off-scale",
                e.pitch
            );
        }
    }

    #[test]
    fn tempo_map_varies_with_commit_cadence() {
        let mut cs = commits();
        // A burst of rapid commits, then a huge gap.
        for (i, c) in cs.iter_mut().enumerate() {
            c.timestamp = 1_600_000_000
                + if i < 5 { i as i64 * 3_600 } else { 100 * 86_400 + i as i64 * 86_400 };
        }
        let s = compose_history(0xFEED, "main", &cs, &authors(), 10, &params());
        assert!(s.tempo_map.len() > 1, "tempo should modulate");
    }
}
