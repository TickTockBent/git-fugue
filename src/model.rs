//! Intermediate representations shared across the pipeline (spec §3.1).

use std::path::PathBuf;

/// Language families, used for instrument selection (spec §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lang {
    Systems,   // Rust / C / C++      -> cello / low strings
    Python,    //                     -> acoustic piano
    JsTs,      //                     -> electric piano
    Go,        //                     -> marimba
    Jvm,       // Java / Kotlin / C#  -> french horn
    Markup,    // HTML / CSS          -> warm pad
    Docs,      // Markdown / text     -> ambient pad
    Config,    // YAML / TOML / JSON  -> pizzicato strings
    Shell,     // shell / CI          -> light percussion (woodblock for now)
    Other,     //                     -> nylon guitar
}

impl Lang {
    pub fn from_path(path: &std::path::Path) -> Lang {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "rs" | "c" | "h" | "cpp" | "cc" | "hpp" | "cxx" => Lang::Systems,
            "py" | "pyi" => Lang::Python,
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => Lang::JsTs,
            "go" => Lang::Go,
            "java" | "kt" | "kts" | "cs" | "scala" => Lang::Jvm,
            "html" | "htm" | "css" | "scss" | "sass" | "svelte" | "vue" => Lang::Markup,
            "md" | "rst" | "txt" | "adoc" => Lang::Docs,
            "yml" | "yaml" | "toml" | "json" | "ini" | "cfg" | "lock" => Lang::Config,
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" => Lang::Shell,
            _ => {
                if name == "makefile" || name == "dockerfile" || name == "justfile" {
                    Lang::Shell
                } else {
                    Lang::Other
                }
            }
        }
    }

    /// General MIDI program number for this language family (spec §5.4).
    pub fn gm_program(self) -> u8 {
        match self {
            Lang::Systems => 42, // Cello
            Lang::Python => 0,   // Acoustic Grand Piano
            Lang::JsTs => 4,     // Electric Piano 1
            Lang::Go => 12,      // Marimba
            Lang::Jvm => 60,     // French Horn
            Lang::Markup => 89,  // Pad 2 (warm)
            Lang::Docs => 88,    // Pad 1 (new age)
            Lang::Config => 45,  // Pizzicato Strings
            Lang::Shell => 115,  // Woodblock
            Lang::Other => 24,   // Nylon Guitar
        }
    }

    pub fn instrument_name(self) -> &'static str {
        match self {
            Lang::Systems => "cello",
            Lang::Python => "piano",
            Lang::JsTs => "electric piano",
            Lang::Go => "marimba",
            Lang::Jvm => "french horn",
            Lang::Markup => "warm pad",
            Lang::Docs => "ambient pad",
            Lang::Config => "pizzicato strings",
            Lang::Shell => "woodblock",
            Lang::Other => "nylon guitar",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FnUnit {
    pub name_hash: u64,
    pub lines: u32,
    pub nesting: u8,
}

#[derive(Debug, Clone)]
pub struct CodeUnit {
    pub path: PathBuf,
    pub depth: u8,
    pub language: Lang,
    pub loc: u32,
    /// Spec §3.1 IR field; per-function nesting drives tension today,
    /// the file-level max is kept for future mappings.
    #[allow(dead_code)]
    pub nesting_max: u8,
    pub ident_hash: u64,
    pub functions: Vec<FnUnit>,
}

/// Mode-specific input to the composer.
pub enum RepoModel {
    Static { seed: u64, units: Vec<CodeUnit> },
    // History { ... }  -- Phase 2
}

// ---- Composer output ----

pub type Tick = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scale {
    MajorPentatonic,
    MinorPentatonic,
    Dorian,
    Aeolian,
}

impl Scale {
    pub fn degrees(self) -> &'static [u8] {
        match self {
            Scale::MajorPentatonic => &[0, 2, 4, 7, 9],
            Scale::MinorPentatonic => &[0, 3, 5, 7, 10],
            Scale::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Scale::Aeolian => &[0, 2, 3, 5, 7, 8, 10],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scale::MajorPentatonic => "major pentatonic",
            Scale::MinorPentatonic => "minor pentatonic",
            Scale::Dorian => "dorian",
            Scale::Aeolian => "aeolian",
        }
    }

    pub fn parse(s: &str) -> Option<Scale> {
        match s {
            "pentatonic" | "major-pentatonic" => Some(Scale::MajorPentatonic),
            "minor-pentatonic" => Some(Scale::MinorPentatonic),
            "dorian" => Some(Scale::Dorian),
            "aeolian" => Some(Scale::Aeolian),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// Pitch class of the tonic, 0 = C.
    pub root: u8,
}

impl Key {
    pub fn name(self) -> &'static str {
        const NAMES: [&str; 12] = [
            "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
        ];
        NAMES[(self.root % 12) as usize]
    }
}

#[derive(Debug, Clone)]
pub struct Voice {
    pub name: String,
    pub channel: u8,
    pub program: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct NoteEvent {
    pub voice: usize,
    pub pitch: u8,
    pub start: Tick,
    pub dur: Tick,
    pub velocity: u8,
}

pub struct Score {
    pub bpm_base: u16,
    pub key: Key,
    pub scale: Scale,
    pub voices: Vec<Voice>,
    pub events: Vec<NoteEvent>,
    pub tempo_map: Vec<(Tick, u16)>,
    pub liner_notes: Vec<String>,
    pub seed: u64,
}
