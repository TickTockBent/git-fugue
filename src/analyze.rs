//! Analyze stage: raw files -> RepoModel::Static.
//!
//! Function boundaries come from tree-sitter where a grammar is
//! compiled in (Rust, Python, JS/TS); everything else falls back to
//! indentation-based block detection so any text repo renders (spec §5.3).
//! Nesting depth is always the indentation proxy — cheap and
//! language-agnostic (spec §5.1, open question 5).

use std::collections::BTreeMap;
use std::path::Path;

use crate::extract::{RawCommit, RawFile};
use crate::model::{Author, CodeUnit, CommitNode, FnUnit, Lang, RepoModel};
use crate::rng::fnv1a;

const MAX_FILE_BYTES: usize = 1_000_000;

pub fn build_units(files: &[RawFile]) -> Vec<CodeUnit> {
    let mut units: Vec<CodeUnit> = files
        .iter()
        .filter(|f| f.content.len() <= MAX_FILE_BYTES && !is_binary(&f.content))
        .filter_map(analyze_file)
        .collect();
    units.sort_by(|a, b| a.path.cmp(&b.path));
    units
}

fn is_binary(content: &[u8]) -> bool {
    content.iter().take(8000).any(|b| *b == 0)
}

fn analyze_file(file: &RawFile) -> Option<CodeUnit> {
    let text = String::from_utf8_lossy(&file.content);
    let lines: Vec<&str> = text.lines().collect();
    let loc = lines.iter().filter(|l| !l.trim().is_empty()).count() as u32;
    if loc == 0 {
        return None;
    }
    let language = Lang::from_path(&file.path);
    let depth = file.path.components().count().saturating_sub(1) as u8;
    let nesting_max = max_indent_level(&lines);
    let ident_hash = identifier_hash(&text);

    let mut functions = tree_sitter_functions(&file.path, &text, &lines)
        .unwrap_or_else(|| indent_functions(&lines));
    if functions.is_empty() {
        // Whole file as a single unit: guarantees every file sings.
        functions.push(FnUnit {
            name_hash: fnv1a(file.path.to_string_lossy().as_bytes()),
            lines: loc,
            nesting: nesting_max,
        });
    }

    Some(CodeUnit {
        path: file.path.clone(),
        depth,
        language,
        loc,
        nesting_max,
        ident_hash,
        functions,
    })
}

/// Rolling FNV-1a over identifier-like tokens. This is the stream that
/// drives contour selection and scale-degree choices (spec §5.1).
fn identifier_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut in_ident = false;
    for ch in text.bytes() {
        let is_ident = ch.is_ascii_alphanumeric() || ch == b'_';
        if is_ident {
            h ^= ch as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            in_ident = true;
        } else if in_ident {
            h = h.rotate_left(7);
            in_ident = false;
        }
    }
    h
}

// ---- indentation metrics ----

fn indent_width(line: &str) -> Option<usize> {
    if line.trim().is_empty() {
        return None;
    }
    let mut w = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => w += 1,
            '\t' => w += 4,
            _ => break,
        }
    }
    Some(w)
}

/// Smallest positive indent step in the file; the "indent unit".
fn indent_unit(lines: &[&str]) -> usize {
    let mut unit = usize::MAX;
    for l in lines {
        if let Some(w) = indent_width(l)
            && w > 0 && w < unit {
                unit = w;
            }
    }
    if unit == usize::MAX || unit == 0 {
        4
    } else {
        unit
    }
}

fn max_indent_level(lines: &[&str]) -> u8 {
    let unit = indent_unit(lines);
    let max = lines
        .iter()
        .filter_map(|l| indent_width(l))
        .max()
        .unwrap_or(0);
    (max / unit).min(255) as u8
}

/// Fallback function detection: a top-level line followed by deeper
/// indentation opens a block; the block runs until the next such opener.
fn indent_functions(lines: &[&str]) -> Vec<FnUnit> {
    let unit = indent_unit(lines);
    let mut blocks: Vec<(usize, usize)> = Vec::new(); // (start, end) line idx
    let mut open: Option<usize> = None;
    for i in 0..lines.len() {
        let w = match indent_width(lines[i]) {
            Some(w) => w,
            None => continue,
        };
        if w == 0 {
            let opens_block = lines[i + 1..]
                .iter()
                .find_map(|l| indent_width(l))
                .map(|next| next > 0)
                .unwrap_or(false);
            if let Some(s) = open.take() {
                blocks.push((s, i));
            }
            if opens_block {
                open = Some(i);
            }
        }
    }
    if let Some(s) = open {
        blocks.push((s, lines.len()));
    }
    blocks
        .into_iter()
        .map(|(s, e)| block_to_fn(lines, s, e, unit))
        .collect()
}

fn block_to_fn(lines: &[&str], start: usize, end: usize, unit: usize) -> FnUnit {
    let body = &lines[start..end];
    let loc = body.iter().filter(|l| !l.trim().is_empty()).count() as u32;
    let nesting = body
        .iter()
        .filter_map(|l| indent_width(l))
        .max()
        .unwrap_or(0)
        / unit;
    FnUnit {
        name_hash: fnv1a(lines[start].trim().as_bytes()),
        lines: loc.max(1),
        nesting: nesting.min(255) as u8,
    }
}

// ---- history mode ----

/// Bot detection (spec §6.3): dependabot, renovate, `*[bot]`.
pub fn is_bot(name: &str, email: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let e = email.to_ascii_lowercase();
    n.contains("[bot]")
        || e.contains("[bot]")
        || n.starts_with("dependabot")
        || n.starts_with("renovate")
        || e.starts_with("dependabot")
        || e.starts_with("renovate")
}

/// Build RepoModel::History from the raw first-parent log.
/// `compress` folds every K consecutive commits into one bar (spec §6.1:
/// at large N, one bar per K commits instead of a six-hour piece).
pub fn build_history_model(
    raw: &[RawCommit],
    identity_seed: u64,
    branch: String,
    total_commits: u32,
    compress: u32,
) -> RepoModel {
    // Authors keyed by normalized email; empty emails fall back to name.
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut authors: Vec<Author> = Vec::new();
    let mut author_of = Vec::with_capacity(raw.len());
    for c in raw {
        let key = if c.author_email.is_empty() {
            c.author_name.to_ascii_lowercase()
        } else {
            c.author_email.clone()
        };
        let id = *index.entry(key.clone()).or_insert_with(|| {
            authors.push(Author {
                email: key,
                name: c.author_name.clone(),
                commits: 0,
                is_bot: is_bot(&c.author_name, &c.author_email),
            });
            authors.len() - 1
        });
        authors[id].commits += 1;
        author_of.push(id);
    }

    let k = compress.max(1) as usize;
    let mut commits = Vec::with_capacity(raw.len().div_ceil(k));
    for group in raw.chunks(k) {
        let gi = commits.len() * k;
        let last = group.last().unwrap();
        // Majority author of the group; ties go to the earliest seen.
        let mut counts: BTreeMap<usize, u32> = BTreeMap::new();
        for (j, _) in group.iter().enumerate() {
            *counts.entry(author_of[gi + j]).or_insert(0) += 1;
        }
        let mut best = author_of[gi];
        let mut best_count = 0;
        for (j, _) in group.iter().enumerate() {
            let id = author_of[gi + j];
            if counts[&id] > best_count {
                best = id;
                best_count = counts[&id];
            }
        }
        commits.push(CommitNode {
            hash: u64::from_str_radix(&last.hash[..16.min(last.hash.len())], 16)
                .unwrap_or_else(|_| fnv1a(last.hash.as_bytes())),
            short: last.hash.chars().take(7).collect(),
            author_id: best,
            timestamp: last.timestamp,
            diff_magnitude: group
                .iter()
                .map(|c| c.diff_magnitude as u64)
                .sum::<u64>()
                .min(u32::MAX as u64) as u32,
            is_merge: group.iter().any(|c| c.is_merge),
            folded: group.len() as u32,
        });
    }

    RepoModel::History {
        identity_seed,
        branch,
        commits,
        authors,
        total_commits,
    }
}

// ---- tree-sitter ----

fn tree_sitter_functions(path: &Path, text: &str, lines: &[&str]) -> Option<Vec<FnUnit>> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let (language, kinds): (tree_sitter::Language, &[&str]) = match ext.as_str() {
        "rs" => (tree_sitter_rust::LANGUAGE.into(), &["function_item"]),
        "py" => (
            tree_sitter_python::LANGUAGE.into(),
            &["function_definition"],
        ),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => (
            // The JS grammar covers the shared function syntax well enough
            // for boundary detection in TS too, day one.
            tree_sitter_javascript::LANGUAGE.into(),
            &["function_declaration", "method_definition", "generator_function_declaration"],
        ),
        _ => return None,
    };

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;
    let unit = indent_unit(lines);

    let mut fns = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if kinds.contains(&node.kind()) {
            let start = node.start_position().row;
            let end = (node.end_position().row + 1).min(lines.len());
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .unwrap_or("anonymous");
            let mut f = block_to_fn(lines, start.min(lines.len().saturating_sub(1)), end, unit);
            f.name_hash = fnv1a(name.as_bytes());
            fns.push(f);
        }
        for i in (0..node.child_count() as u32).rev() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
    }
    // The stack walk (children pushed in reverse) yields document order.
    Some(fns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn raw(path: &str, content: &str) -> RawFile {
        RawFile {
            path: PathBuf::from(path),
            content: content.as_bytes().to_vec(),
        }
    }

    #[test]
    fn rust_functions_found() {
        let f = raw(
            "src/lib.rs",
            "fn alpha() {\n    let x = 1;\n}\n\nfn beta() {\n    if true {\n        loop {}\n    }\n}\n",
        );
        let units = build_units(&[f]);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].functions.len(), 2);
        assert_eq!(units[0].language, Lang::Systems);
    }

    #[test]
    fn indent_fallback_finds_blocks() {
        let f = raw(
            "notes.zig",
            "block one:\n    inner\n    inner\nplain line\nblock two:\n    inner\n",
        );
        let units = build_units(&[f]);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].functions.len(), 2);
    }

    #[test]
    fn binary_skipped_empty_skipped() {
        let bin = RawFile {
            path: PathBuf::from("a.bin"),
            content: vec![0, 1, 2, 3],
        };
        let empty = raw("empty.txt", "\n\n");
        assert!(build_units(&[bin, empty]).is_empty());
    }

    #[test]
    fn determinism() {
        let f = raw("src/main.py", "def f():\n    return 1\n\ndef g():\n    pass\n");
        let a = build_units(&[raw("src/main.py", "def f():\n    return 1\n\ndef g():\n    pass\n")]);
        let b = build_units(&[f]);
        assert_eq!(a[0].ident_hash, b[0].ident_hash);
        assert_eq!(a[0].functions.len(), b[0].functions.len());
    }
}
