//! Render stage: Score -> Standard MIDI File bytes.
//!
//! Format 1 SMF: track 0 carries tempo/meta (including engine version
//! and seed, spec §9), one track per voice after that.

use anyhow::Result;
use midly::num::{u15, u24, u28, u4, u7};
use midly::{
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
};

use crate::compose::TPQ;
use crate::model::Score;

pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn render_midi(score: &Score) -> Result<Vec<u8>> {
    // Owned strings must outlive the Smf, which borrows byte slices.
    let stamp = format!(
        "gitfugue v{} seed={:016x} key={} {} bpm={}",
        ENGINE_VERSION,
        score.seed,
        score.key.name(),
        score.scale.name(),
        score.bpm_base
    );
    let voice_names: Vec<String> = score.voices.iter().map(|v| v.name.clone()).collect();

    let mut smf = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(u15::from(TPQ as u16)),
    ));

    // Track 0: meta.
    let mut meta = Vec::new();
    meta.push(TrackEvent {
        delta: u28::from(0),
        kind: TrackEventKind::Meta(MetaMessage::Text(stamp.as_bytes())),
    });
    meta.push(TrackEvent {
        delta: u28::from(0),
        kind: TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
    });
    let mut last_tick = 0u32;
    for (tick, bpm) in &score.tempo_map {
        meta.push(TrackEvent {
            delta: u28::from(tick - last_tick),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from(
                60_000_000 / *bpm as u32,
            ))),
        });
        last_tick = *tick;
    }
    meta.push(TrackEvent {
        delta: u28::from(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    smf.tracks.push(meta);

    // One track per voice.
    for (vi, voice) in score.voices.iter().enumerate() {
        let ch = u4::from(voice.channel);
        // (tick, order, kind): order 0 = note-off, 1 = program change,
        // 2 = note-on, so repeated pitches never stick and timbre
        // switches land before the notes they color.
        let mut moments: Vec<(u32, u8, MidiMessage)> = Vec::new();
        for (tick, _, program) in score.program_changes.iter().filter(|(_, v, _)| *v == vi) {
            moments.push((
                *tick,
                1,
                MidiMessage::ProgramChange {
                    program: u7::from((*program).min(127)),
                },
            ));
        }
        for e in score.events.iter().filter(|e| e.voice == vi) {
            moments.push((
                e.start,
                2,
                MidiMessage::NoteOn {
                    key: u7::from(e.pitch.min(127)),
                    vel: u7::from(e.velocity.min(127)),
                },
            ));
            moments.push((
                e.start + e.dur.max(1),
                0,
                MidiMessage::NoteOff {
                    key: u7::from(e.pitch.min(127)),
                    vel: u7::from(0),
                },
            ));
        }
        moments.sort_by_key(|(t, o, m)| (*t, *o, key_of(m)));

        let mut track = Vec::new();
        track.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(voice_names[vi].as_bytes())),
        });
        track.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Midi {
                channel: ch,
                message: MidiMessage::ProgramChange {
                    program: u7::from(voice.program.min(127)),
                },
            },
        });
        let mut last = 0u32;
        for (tick, _, msg) in moments {
            track.push(TrackEvent {
                delta: u28::from(tick - last),
                kind: TrackEventKind::Midi {
                    channel: ch,
                    message: msg,
                },
            });
            last = tick;
        }
        track.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(track);
    }

    let mut bytes = Vec::new();
    smf.write(&mut bytes)
        .map_err(|e| anyhow::anyhow!("MIDI write failed: {e}"))?;
    Ok(bytes)
}

fn key_of(m: &MidiMessage) -> u8 {
    match m {
        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => key.as_int(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Key, NoteEvent, Scale, Voice};

    fn tiny_score() -> Score {
        Score {
            bpm_base: 90,
            key: Key { root: 0 },
            scale: Scale::MajorPentatonic,
            voices: vec![Voice {
                name: "piano".into(),
                channel: 0,
                program: 0,
            }],
            events: vec![
                NoteEvent { voice: 0, pitch: 60, start: 0, dur: 480, velocity: 80 },
                NoteEvent { voice: 0, pitch: 64, start: 480, dur: 480, velocity: 80 },
            ],
            tempo_map: vec![(0, 90)],
            program_changes: vec![],
            liner_notes: vec![],
            seed: 42,
        }
    }

    #[test]
    fn roundtrips_through_midly() {
        let bytes = render_midi(&tiny_score()).unwrap();
        let parsed = Smf::parse(&bytes).unwrap();
        assert_eq!(parsed.tracks.len(), 2);
    }

    #[test]
    fn byte_identical_rendering() {
        let a = render_midi(&tiny_score()).unwrap();
        let b = render_midi(&tiny_score()).unwrap();
        assert_eq!(a, b);
    }
}
