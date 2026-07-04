//! Compose stage: RepoModel -> Score. All musical decisions live here.
//!
//! Static mode (spec §5): sorted walk of code units, sections from
//! top-level directories, phrases from functions, tension from nesting
//! depth, drone harmony under the melody. Deterministic by
//! construction: hierarchical RNG scopes, sorted collections only.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    CodeUnit, Key, Lang, NoteEvent, RepoModel, Scale, Score, Tick, Voice,
};
use crate::rng::Rng;
use crate::theory::{pick_positions, MelodyWalk, CONTOURS};

pub const TPQ: u32 = 480; // ticks per quarter note
pub(crate) const SLOT: u32 = TPQ / 4; // 16th-note grid
pub(crate) const BAR: u32 = TPQ * 4; // 4/4

pub struct ComposeParams {
    pub scale: Option<Scale>,
    pub bpm: Option<u16>,
    pub duration_secs: Option<u32>,
}

struct PhraseSpec {
    unit: usize,
    func: usize,
    bars: u32,
}

struct Section {
    name: String,
    unit_indices: Vec<usize>,
    loc: u64,
    bars: u32,
}

pub fn compose(model: &RepoModel, params: &ComposeParams) -> Score {
    match model {
        RepoModel::Static { seed, units } => compose_static(*seed, units, params),
        RepoModel::History {
            identity_seed,
            branch,
            commits,
            authors,
            total_commits,
        } => crate::compose_history::compose_history(
            *identity_seed,
            branch,
            commits,
            authors,
            *total_commits,
            params,
        ),
    }
}

fn compose_static(seed: u64, units: &[CodeUnit], params: &ComposeParams) -> Score {
    let root = Rng::new(seed);
    let mut notes = Vec::new();
    let mut liner = Vec::new();

    // ---- global parameters, seeded from the HEAD tree hash ----
    let mut grng = root.child("global");
    let key = Key {
        root: grng.below(12) as u8,
    };
    let scale = params.scale.unwrap_or_else(|| {
        // The seed picks the mode; config/flags can pin it (spec §4, §7).
        if grng.weighted(&[60, 40]) == 0 {
            Scale::MajorPentatonic
        } else {
            Scale::MinorPentatonic
        }
    });
    let bpm = params.bpm.unwrap_or_else(|| grng.range(70, 110) as u16);
    let duration = params
        .duration_secs
        .unwrap_or_else(|| grng.range(90, 180) as u32);
    // bars = seconds * (beats/sec) / (beats/bar)
    let total_bars = ((duration * bpm as u32) / 240).max(8);

    liner.push(format!(
        "key: {} {}  bpm: {}  target: {}s ({} bars)",
        key.name(),
        scale.name(),
        bpm,
        duration,
        total_bars
    ));

    // ---- sections: top-level directories, bars by LOC weight ----
    let sections = plan_sections(units, total_bars);
    for s in &sections {
        liner.push(format!(
            "section {}: {} bars ({} loc, {} files)",
            s.name,
            s.bars,
            s.loc,
            s.unit_indices.len()
        ));
    }

    // ---- plan phrases for every section, then assign voices ----
    let mut plans: Vec<(usize, Vec<PhraseSpec>)> = Vec::new();
    for (si, section) in sections.iter().enumerate() {
        let specs = plan_phrases(section, units, &mut liner);
        plans.push((si, specs));
    }

    let langs: BTreeSet<Lang> = plans
        .iter()
        .flat_map(|(_, specs)| specs.iter())
        .map(|p| units[p.unit].language)
        .collect();
    let mut voices = Vec::new();
    let mut voice_of: BTreeMap<Lang, usize> = BTreeMap::new();
    let mut channel = 0u8;
    for lang in &langs {
        if channel == 9 {
            channel = 10; // channel 10 (0-indexed 9) is GM percussion
        }
        voice_of.insert(*lang, voices.len());
        liner.push(format!("voice {}: {}", voices.len(), lang.instrument_name()));
        voices.push(Voice {
            name: lang.instrument_name().to_string(),
            channel,
            program: lang.gm_program(),
        });
        channel += 1;
    }
    if channel == 9 {
        channel = 10;
    }
    let drone_voice = voices.len();
    voices.push(Voice {
        name: "drone".to_string(),
        channel,
        program: 89, // Pad 2 (warm)
    });

    // ---- generate ----
    let mut tick: Tick = 0;
    let mut transitions: Vec<Tick> = Vec::new();
    for (si, specs) in &plans {
        let section = &sections[*si];
        if *si > 0 {
            // Transition figure between sections: melody breathes,
            // harmony walks fifth -> root.
            transitions.push(tick);
            tick += BAR;
        }
        let srng = root.child(&format!("section:{}", section.name));
        for spec in specs {
            let unit = &units[spec.unit];
            let f = &unit.functions[spec.func];
            let mut prng = srng.child_u64(unit.ident_hash ^ f.name_hash);
            let voice = voice_of[&unit.language];
            gen_phrase(
                &mut notes, &mut prng, key, scale, unit, f, spec.bars, tick, voice,
            );
            tick += spec.bars * BAR;
        }
    }
    let end = tick;

    // ---- drone harmony: root + fifth, restruck every two bars ----
    gen_drone(&mut notes, key, end, &transitions, drone_voice);

    notes.sort_by_key(|e| (e.start, e.voice, e.pitch));

    Score {
        bpm_base: bpm,
        key,
        scale,
        voices,
        events: notes,
        tempo_map: vec![(0, bpm)],
        liner_notes: liner,
        seed,
    }
}

fn plan_sections(units: &[CodeUnit], total_bars: u32) -> Vec<Section> {
    let mut by_top: BTreeMap<String, (Vec<usize>, u64)> = BTreeMap::new();
    for (i, u) in units.iter().enumerate() {
        let top = if u.path.components().count() > 1 {
            u.path
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".into())
        } else {
            ".".to_string()
        };
        let e = by_top.entry(top).or_insert_with(|| (Vec::new(), 0));
        e.0.push(i);
        e.1 += u.loc as u64;
    }
    let total_loc: u64 = by_top.values().map(|(_, l)| *l).sum::<u64>().max(1);
    let mut sections: Vec<Section> = by_top
        .into_iter()
        .map(|(name, (idx, loc))| Section {
            name,
            unit_indices: idx,
            loc,
            bars: 0,
        })
        .collect();
    // Proportional allocation with a floor of 1 bar per section.
    let mut allocated = 0u32;
    for s in &mut sections {
        s.bars = ((s.loc as u128 * total_bars as u128) / total_loc as u128).max(1) as u32;
        allocated += s.bars;
    }
    // Trim overshoot from the largest sections.
    while allocated > total_bars {
        if let Some(s) = sections.iter_mut().filter(|s| s.bars > 1).max_by_key(|s| s.bars) {
            s.bars -= 1;
            allocated -= 1;
        } else {
            break;
        }
    }
    sections
}

/// Turn a section's functions into a bar-budgeted phrase list.
/// Over budget: stride-sample (spec §5.2). Under budget: da capo.
fn plan_phrases(section: &Section, units: &[CodeUnit], liner: &mut Vec<String>) -> Vec<PhraseSpec> {
    let mut all: Vec<PhraseSpec> = Vec::new();
    for &ui in &section.unit_indices {
        for (fi, f) in units[ui].functions.iter().enumerate() {
            all.push(PhraseSpec {
                unit: ui,
                func: fi,
                bars: (f.lines / 8).clamp(1, 4),
            });
        }
    }
    if all.is_empty() {
        return all;
    }
    let budget = section.bars;
    let needed: u32 = all.iter().map(|p| p.bars).sum();
    if needed > budget {
        let stride = needed.div_ceil(budget).max(1) as usize;
        let before = all.len();
        all = all
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % stride == 0)
            .map(|(_, p)| p)
            .collect();
        let mut sum: u32 = all.iter().map(|p| p.bars).sum();
        while sum > budget && all.len() > 1 {
            sum -= all.pop().map(|p| p.bars).unwrap_or(0);
        }
        liner.push(format!(
            "  {}: sampled {} of {} motifs (stride {})",
            section.name,
            all.len(),
            before,
            stride
        ));
    } else if needed < budget {
        // Repeat material rather than padding with silence.
        let base = all.len();
        let mut sum = needed;
        let mut i = 0usize;
        while sum + all[i % base].bars <= budget {
            let src = &all[i % base];
            all.push(PhraseSpec {
                unit: src.unit,
                func: src.func,
                bars: src.bars,
            });
            sum += all.last().unwrap().bars;
            i += 1;
            if i > 4096 {
                break;
            }
        }
    }
    all
}

/// Preferred register center per instrument family.
fn lang_center(lang: Lang) -> i32 {
    match lang {
        Lang::Systems => 48,
        Lang::Python => 64,
        Lang::JsTs => 64,
        Lang::Go => 67,
        Lang::Jvm => 55,
        Lang::Markup => 60,
        Lang::Docs => 57,
        Lang::Config => 62,
        Lang::Shell => 72,
        Lang::Other => 60,
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_phrase(
    notes: &mut Vec<NoteEvent>,
    rng: &mut Rng,
    key: Key,
    scale: Scale,
    unit: &CodeUnit,
    f: &crate::model::FnUnit,
    bars: u32,
    start: Tick,
    voice: usize,
) {
    // Tension from nesting depth (spec §5.1): the complexity proxy.
    let t = (f.nesting as f64 / 8.0).min(1.0);
    // High tension borrows the parallel minor (spec §4).
    let local_scale = if t > 0.8 && scale == Scale::MajorPentatonic {
        Scale::MinorPentatonic
    } else {
        scale
    };
    let contour = CONTOURS[(f.name_hash % CONTOURS.len() as u64) as usize];
    // Directory depth chooses the octave within the voice range (spec §5.1).
    let center =
        (lang_center(unit.language) + ((unit.depth % 3) as i32 - 1) * 12).clamp(36, 84) as u8;
    let mut walk = MelodyWalk::new(key, local_scale, center);

    let total_slots = bars * 16;
    for bar in 0..bars {
        let last_bar = bar == bars - 1;
        // Note density scales with tension: 4..=10 notes per bar.
        let mut density = 4 + (t * 6.0).round() as u32;
        if last_bar {
            density = density.saturating_sub(1).max(2); // breath at phrase end
        }
        let positions = pick_positions(rng, density, last_bar);
        for (pi, &slot) in positions.iter().enumerate() {
            let gap = if pi + 1 < positions.len() {
                positions[pi + 1] - slot
            } else if last_bar {
                (16 - slot).min(4) // release at phrase end
            } else {
                16 - slot
            };
            let progress = (bar * 16 + slot) as f64 / total_slots as f64;
            let pitch = walk.step(rng, contour.target(progress));

            // Velocity: strong-beat accents plus tension-scaled variance.
            let mut vel = 66.0 + t * 10.0;
            if slot == 0 || slot == 8 {
                vel += 12.0;
            } else if slot % 4 == 0 {
                vel += 6.0;
            }
            let var = 3.0 + t * 14.0;
            vel += rng.f64() * 2.0 * var - var;
            let vel = vel.clamp(30.0, 115.0) as u8;

            let mut dur_slots = gap.max(1);
            if rng.f64() < 0.25 && dur_slots > 1 {
                dur_slots = (dur_slots * 3) / 4; // light articulation
            }
            let note_start = start + bar * BAR + slot * SLOT;

            // High tension admits a chromatic neighbor tone (spec §4):
            // main - neighbor - main, an in-place mordent figure.
            if t > 0.6 && rng.f64() < (t - 0.6) && dur_slots >= 3 && pitch > 1 {
                let neighbor = if rng.weighted(&[50, 50]) == 0 {
                    pitch - 1
                } else {
                    pitch + 1
                };
                let d1 = dur_slots - 2;
                notes.push(NoteEvent {
                    voice,
                    pitch,
                    start: note_start,
                    dur: d1 * SLOT,
                    velocity: vel,
                });
                notes.push(NoteEvent {
                    voice,
                    pitch: neighbor,
                    start: note_start + d1 * SLOT,
                    dur: SLOT,
                    velocity: vel.saturating_sub(10),
                });
                notes.push(NoteEvent {
                    voice,
                    pitch,
                    start: note_start + (d1 + 1) * SLOT,
                    dur: SLOT,
                    velocity: vel.saturating_sub(6),
                });
            } else {
                notes.push(NoteEvent {
                    voice,
                    pitch,
                    start: note_start,
                    dur: dur_slots * SLOT,
                    velocity: vel,
                });
            }
        }
    }
}

/// Sustained root + fifth under everything; a fifth->root walk in
/// transition bars so section changes are audible as cadences.
fn gen_drone(
    notes: &mut Vec<NoteEvent>,
    key: Key,
    end: Tick,
    transitions: &[Tick],
    voice: usize,
) {
    let root = 36 + key.root; // octave 2
    let fifth = root + 7;
    let mut bar_start: Tick = 0;
    while bar_start < end {
        if transitions.contains(&bar_start) {
            // Cadence figure: fifth (half) -> root (half), slightly louder.
            notes.push(NoteEvent {
                voice,
                pitch: fifth,
                start: bar_start,
                dur: BAR / 2,
                velocity: 52,
            });
            notes.push(NoteEvent {
                voice,
                pitch: root,
                start: bar_start + BAR / 2,
                dur: BAR / 2,
                velocity: 56,
            });
            bar_start += BAR;
            continue;
        }
        // Hold root+fifth for two bars (or to the next transition/end).
        let mut dur = 2 * BAR;
        for &tr in transitions {
            if tr > bar_start && tr < bar_start + dur {
                dur = tr - bar_start;
            }
        }
        if bar_start + dur > end {
            dur = end - bar_start;
        }
        for (p, v) in [(root, 44u8), (fifth, 38u8)] {
            notes.push(NoteEvent {
                voice,
                pitch: p,
                start: bar_start,
                dur,
                velocity: v,
            });
        }
        bar_start += dur;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FnUnit;
    use std::path::PathBuf;

    fn unit(path: &str, lang_hint: &str, loc: u32, nesting: u8, fns: usize) -> CodeUnit {
        let path = PathBuf::from(format!("{path}.{lang_hint}"));
        CodeUnit {
            depth: path.components().count().saturating_sub(1) as u8,
            language: Lang::from_path(&path),
            loc,
            nesting_max: nesting,
            ident_hash: crate::rng::fnv1a(path.to_string_lossy().as_bytes()),
            functions: (0..fns)
                .map(|i| FnUnit {
                    name_hash: i as u64 * 7919 + 13,
                    lines: 10 + i as u32 * 5,
                    nesting,
                })
                .collect(),
            path,
        }
    }

    fn model() -> RepoModel {
        RepoModel::Static {
            seed: 0xDEADBEEF,
            units: vec![
                unit("src/main", "rs", 120, 3, 4),
                unit("src/util", "rs", 60, 5, 2),
                unit("docs/readme", "md", 40, 1, 1),
                unit("scripts/build", "sh", 25, 2, 2),
            ],
        }
    }

    #[test]
    fn deterministic_score() {
        let p = ComposeParams {
            scale: None,
            bpm: None,
            duration_secs: None,
        };
        let a = compose(&model(), &p);
        let b = compose(&model(), &p);
        assert_eq!(a.bpm_base, b.bpm_base);
        assert_eq!(a.events.len(), b.events.len());
        for (x, y) in a.events.iter().zip(&b.events) {
            assert_eq!((x.pitch, x.start, x.dur, x.velocity), (y.pitch, y.start, y.dur, y.velocity));
        }
    }

    #[test]
    fn melody_pitches_in_scale_or_neighbors() {
        let p = ComposeParams {
            scale: Some(Scale::MajorPentatonic),
            bpm: Some(100),
            duration_secs: Some(60),
        };
        let s = compose(&model(), &p);
        let drone_voice = s.voices.len() - 1;
        let major: &[u8] = Scale::MajorPentatonic.degrees();
        let minor: &[u8] = Scale::MinorPentatonic.degrees();
        let mut off_scale = 0usize;
        let mut total = 0usize;
        for e in s.events.iter().filter(|e| e.voice != drone_voice) {
            total += 1;
            let pc = ((e.pitch as i32 - s.key.root as i32).rem_euclid(12)) as u8;
            if !major.contains(&pc) && !minor.contains(&pc) {
                off_scale += 1; // only allowed as neighbor tones
            }
        }
        assert!(total > 50, "expected a real number of notes, got {total}");
        assert!(
            (off_scale as f64) < (total as f64) * 0.1,
            "too many off-scale notes: {off_scale}/{total}"
        );
    }

    #[test]
    fn respects_overrides() {
        let p = ComposeParams {
            scale: Some(Scale::Dorian),
            bpm: Some(99),
            duration_secs: Some(45),
        };
        let s = compose(&model(), &p);
        assert_eq!(s.bpm_base, 99);
        assert_eq!(s.scale, Scale::Dorian);
    }

    #[test]
    fn different_seeds_differ() {
        let p = ComposeParams {
            scale: None,
            bpm: None,
            duration_secs: None,
        };
        let a = compose(&model(), &p);
        let b = match model() {
            RepoModel::Static { units, .. } => {
                compose(&RepoModel::Static { seed: 0xCAFED00D, units }, &p)
            }
            _ => unreachable!(),
        };
        let sig_a: Vec<u8> = a.events.iter().take(40).map(|e| e.pitch).collect();
        let sig_b: Vec<u8> = b.events.iter().take(40).map(|e| e.pitch).collect();
        assert_ne!(sig_a, sig_b, "different seeds must produce different songs");
    }
}
