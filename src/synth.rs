//! WAV rendering via rustysynth and the embedded soundfont (spec §8).
//!
//! TimGM6mb by Tim Brechbill ships inside the binary so `--format wav`
//! works with zero setup; `--soundfont` swaps in any other SF2.
//! See assets/SOUNDFONT-LICENSE.md for the soundfont's license (GPL-2.0).

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};

const EMBEDDED_SF2: &[u8] = include_bytes!("../assets/TimGM6mb.sf2");
pub const SAMPLE_RATE: u32 = 44_100;
/// Extra tail after the last event so releases and decays ring out.
const TAIL_SECONDS: f64 = 1.5;

/// Synthesize a rendered SMF into interleaved-channel sample buffers.
pub fn synthesize(midi_bytes: &[u8], soundfont: Option<&Path>) -> Result<(Vec<f32>, Vec<f32>)> {
    let sf_bytes: Vec<u8> = match soundfont {
        Some(p) => std::fs::read(p).with_context(|| format!("reading soundfont {}", p.display()))?,
        None => EMBEDDED_SF2.to_vec(),
    };
    let sound_font = Arc::new(
        SoundFont::new(&mut Cursor::new(sf_bytes))
            .map_err(|e| anyhow::anyhow!("invalid soundfont: {e}"))?,
    );
    let midi = Arc::new(
        MidiFile::new(&mut Cursor::new(midi_bytes))
            .map_err(|e| anyhow::anyhow!("internal MIDI rejected by synth: {e}"))?,
    );
    let settings = SynthesizerSettings::new(SAMPLE_RATE as i32);
    let synthesizer = Synthesizer::new(&sound_font, &settings)
        .map_err(|e| anyhow::anyhow!("synthesizer init failed: {e}"))?;
    let mut sequencer = MidiFileSequencer::new(synthesizer);
    sequencer.play(&midi, false);

    let samples = ((midi.get_length() + TAIL_SECONDS) * SAMPLE_RATE as f64) as usize;
    let mut left = vec![0f32; samples];
    let mut right = vec![0f32; samples];
    sequencer.render(&mut left, &mut right);
    Ok((left, right))
}

/// Render a Standard MIDI File to 16-bit stereo PCM WAV bytes.
pub fn render_wav(midi_bytes: &[u8], soundfont: Option<&Path>) -> Result<Vec<u8>> {
    let (left, right) = synthesize(midi_bytes, soundfont)?;
    Ok(write_wav(&left, &right))
}

fn write_wav(left: &[f32], right: &[f32]) -> Vec<u8> {
    let frames = left.len().min(right.len());
    let data_len = (frames * 4) as u32; // 2 channels x 2 bytes
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 4).to_le_bytes()); // byte rate
    out.extend_from_slice(&4u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for ch in [left, right] {
            let s = (ch[i].clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    out
}

/// Play rendered audio on the default output device (spec §7 --play).
#[cfg(feature = "playback")]
pub fn play(left: Vec<f32>, right: Vec<f32>) -> Result<()> {
    use rodio::buffer::SamplesBuffer;
    let frames = left.len().min(right.len());
    let mut interleaved = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        interleaved.push(left[i]);
        interleaved.push(right[i]);
    }
    let stream = rodio::OutputStreamBuilder::open_default_stream()
        .map_err(|e| anyhow::anyhow!("no audio output device: {e}"))?;
    let sink = rodio::Sink::connect_new(stream.mixer());
    sink.append(SamplesBuffer::new(2, SAMPLE_RATE, interleaved));
    sink.sleep_until_end();
    Ok(())
}

#[cfg(not(feature = "playback"))]
pub fn play(_left: Vec<f32>, _right: Vec<f32>) -> Result<()> {
    anyhow::bail!(
        "this binary was built without playback support; rebuild with \
         `cargo build --features playback` (requires an audio backend, \
         e.g. ALSA headers on Linux) or open the rendered file in a player"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Key, NoteEvent, Scale, Score, Voice};
    use crate::render::render_midi;

    fn tiny_score() -> Score {
        Score {
            bpm_base: 120,
            key: Key { root: 0 },
            scale: Scale::MajorPentatonic,
            voices: vec![Voice { name: "piano".into(), channel: 0, program: 0 }],
            events: vec![
                NoteEvent { voice: 0, pitch: 60, start: 0, dur: 480, velocity: 90 },
                NoteEvent { voice: 0, pitch: 67, start: 480, dur: 480, velocity: 90 },
            ],
            tempo_map: vec![(0, 120)],
            program_changes: vec![],
            liner_notes: vec![],
            seed: 1,
        }
    }

    #[test]
    fn embedded_soundfont_is_valid_sf2() {
        assert!(EMBEDDED_SF2.len() > 1_000_000, "soundfont looks truncated");
        assert_eq!(&EMBEDDED_SF2[..4], b"RIFF");
        assert_eq!(&EMBEDDED_SF2[8..12], b"sfbk");
    }

    #[test]
    fn renders_audible_wav() {
        let midi = render_midi(&tiny_score()).unwrap();
        let wav = render_wav(&midi, None).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        // Two quarter notes at 120bpm = 1s of music + tail.
        assert!(wav.len() > 44 + SAMPLE_RATE as usize, "WAV too short");
        // Not silence: some 16-bit sample must be clearly nonzero.
        let audible = wav[44..]
            .chunks_exact(2)
            .any(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() > 500);
        assert!(audible, "synth produced silence");
    }

    #[test]
    fn wav_render_is_deterministic() {
        let midi = render_midi(&tiny_score()).unwrap();
        let a = render_wav(&midi, None).unwrap();
        let b = render_wav(&midi, None).unwrap();
        assert_eq!(a, b);
    }
}
