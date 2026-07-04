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
use crate::model::{Author, CodeUnit, CommitNode, FnUnit, Lang, RepoModel, LANE_ENSEMBLE};
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

fn collect_authors(raw: &[RawCommit]) -> (Vec<Author>, Vec<usize>) {
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
    (authors, author_of)
}

fn hash_u64(hash: &str) -> u64 {
    u64::from_str_radix(&hash[..16.min(hash.len())], 16)
        .unwrap_or_else(|_| fnv1a(hash.as_bytes()))
}

/// Pull the branch name out of a merge subject, if git or a forge put
/// one there ("Merge branch 'x'", "Merge pull request #1 from user/x").
pub fn branch_from_subject(subject: &str) -> Option<String> {
    if let Some(rest) = subject.strip_prefix("Merge branch '") {
        return rest.split('\'').next().map(|s| s.to_string());
    }
    if let Some(rest) = subject.strip_prefix("Merge remote-tracking branch '") {
        return rest.split('\'').next().map(|s| s.to_string());
    }
    if subject.starts_with("Merge pull request #")
        && let Some(from) = subject.split(" from ").nth(1) {
            let name = from.split_whitespace().next().unwrap_or(from);
            // Strip the owner prefix of "owner/branch".
            return Some(name.split_once('/').map(|(_, b)| b).unwrap_or(name).to_string());
        }
    None
}

/// Build RepoModel::History from the raw log (full DAG, spec §6.2).
///
/// Lane allocation follows the same idea as `git log --graph`: walking
/// oldest-first, a commit extends the lane whose tip is its first
/// parent; a commit whose parent was already extended forks a new
/// lane; a merge closes its second parent's lane. The trunk (HEAD's
/// first-parent chain) always holds lane 0. At most `max_voices`
/// lanes; overflow evicts the least-recently-active branch into the
/// shared ensemble (spec §6.2).
pub fn build_history_model(
    raw: &[RawCommit],
    identity_seed: u64,
    branch: String,
    total_commits: u32,
    head_hash: &str,
    max_voices: u8,
    conflicted: &[bool],
) -> RepoModel {
    let (authors, author_of) = collect_authors(raw);
    let cap = max_voices.clamp(2, 12);

    // Hash -> index, for parent resolution inside the window.
    let by_hash: BTreeMap<&str, usize> =
        raw.iter().enumerate().map(|(i, c)| (c.hash.as_str(), i)).collect();

    // Trunk: HEAD's first-parent chain within the window.
    let mut trunk = vec![false; raw.len()];
    let mut cur = by_hash.get(head_hash).copied();
    while let Some(i) = cur {
        trunk[i] = true;
        cur = raw[i].parents.first().and_then(|p| by_hash.get(p.as_str()).copied());
    }

    #[derive(Clone, Copy)]
    struct LaneState {
        tip: usize,
        last_active: usize,
    }
    let mut lanes: Vec<Option<LaneState>> = vec![None; cap as usize];
    // Commits that are current tips of ensemble-folded branches.
    let mut ensemble_tips: std::collections::BTreeSet<usize> = Default::default();

    let mut commits: Vec<CommitNode> = Vec::with_capacity(raw.len());
    for (i, c) in raw.iter().enumerate() {
        let parents: Vec<usize> = c
            .parents
            .iter()
            .filter_map(|p| by_hash.get(p.as_str()).copied())
            .collect();
        let p1 = parents.first().copied();

        let mut opens_lane = false;
        let mut evicted_lane = None;
        let lane: u8 = if trunk[i] {
            if lanes[0].is_none() {
                opens_lane = false; // the exposition already stated lane 0
            }
            0
        } else if let Some(p) = p1.filter(|p| ensemble_tips.contains(p)) {
            ensemble_tips.remove(&p);
            LANE_ENSEMBLE
        } else if let Some(l) = p1.and_then(|p| {
            // Lane 0 is reserved for the trunk chain: a non-trunk child
            // of the trunk tip is a fork, not a continuation.
            lanes
                .iter()
                .position(|s| matches!(s, Some(st) if st.tip == p))
                .filter(|l| *l != 0)
        }) {
            l as u8 // extends an existing branch lane
        } else {
            // Fork: a new voice. Find a free non-trunk lane, or evict
            // the least-recently-active branch into the ensemble.
            opens_lane = true;
            let free = (1..cap as usize).find(|l| lanes[*l].is_none());
            match free {
                Some(l) => l as u8,
                None => {
                    let l = (1..cap as usize)
                        .min_by_key(|l| lanes[*l].map(|s| s.last_active).unwrap_or(0))
                        .unwrap();
                    if let Some(st) = lanes[l] {
                        ensemble_tips.insert(st.tip);
                        evicted_lane = Some(l as u8);
                    }
                    l as u8
                }
            }
        };

        // A merge releases its other parents' lanes (spec §6.2).
        let mut closes_lane = None;
        for &p in parents.iter().skip(1) {
            if let Some(l) = lanes
                .iter()
                .position(|s| matches!(s, Some(st) if st.tip == p))
                && l as u8 != lane {
                    closes_lane = Some(l as u8);
                    lanes[l] = None;
                }
            ensemble_tips.remove(&p);
        }

        if lane != LANE_ENSEMBLE {
            lanes[lane as usize] = Some(LaneState { tip: i, last_active: i });
        } else {
            ensemble_tips.insert(i);
        }

        commits.push(CommitNode {
            hash: hash_u64(&c.hash),
            short: c.hash.chars().take(7).collect(),
            parents,
            author_id: author_of[i],
            timestamp: c.timestamp,
            diff_magnitude: c.diff_magnitude,
            is_merge: c.is_merge,
            folded: 1,
            lane,
            opens_lane,
            closes_lane,
            fork_name: None,
            conflicted: conflicted.get(i).copied().unwrap_or(false),
            evicted_lane,
        });
    }

    // Second pass: recover branch names. Walk each merge's second
    // parent back along first parents to the fork commit and label it.
    for i in 0..commits.len() {
        if !commits[i].is_merge || commits[i].closes_lane.is_none() {
            continue;
        }
        let name = branch_from_subject(&raw[i].subject);
        let mut cur = commits[i].parents.get(1).copied();
        while let Some(j) = cur {
            if commits[j].opens_lane {
                if commits[j].fork_name.is_none() {
                    commits[j].fork_name = name.clone();
                }
                break;
            }
            cur = commits[j].parents.first().copied();
        }
    }

    RepoModel::History {
        identity_seed,
        branch,
        commits,
        authors,
        total_commits,
    }
}

/// Compressed fallback for huge histories: first-parent walk folded K
/// commits per bar, single voice (entries are inaudible at K:1 anyway).
pub fn build_folded_model(
    raw: &[RawCommit],
    identity_seed: u64,
    branch: String,
    total_commits: u32,
    compress: u32,
) -> RepoModel {
    let (authors, author_of) = collect_authors(raw);
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
            hash: hash_u64(&last.hash),
            short: last.hash.chars().take(7).collect(),
            parents: Vec::new(),
            author_id: best,
            timestamp: last.timestamp,
            diff_magnitude: group
                .iter()
                .map(|c| c.diff_magnitude as u64)
                .sum::<u64>()
                .min(u32::MAX as u64) as u32,
            is_merge: group.iter().any(|c| c.is_merge),
            folded: group.len() as u32,
            lane: 0,
            opens_lane: false,
            closes_lane: None,
            fork_name: None,
            conflicted: false,
            evicted_lane: None,
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
mod lane_tests {
    use super::*;

    fn rc(hash: &str, parents: &[&str], subject: &str) -> RawCommit {
        RawCommit {
            hash: hash.repeat(40 / hash.len().max(1)),
            parents: parents
                .iter()
                .map(|p| p.repeat(40 / p.len().max(1)))
                .collect(),
            author_name: "A".into(),
            author_email: "a@x".into(),
            timestamp: 0,
            diff_magnitude: 10,
            is_merge: parents.len() > 1,
            subject: subject.into(),
        }
    }

    fn lanes_of(model: &RepoModel) -> Vec<u8> {
        match model {
            RepoModel::History { commits, .. } => commits.iter().map(|c| c.lane).collect(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn linear_chain_is_all_trunk() {
        let raw = vec![rc("a1", &[], "init"), rc("b2", &["a1"], "x"), rc("c3", &["b2"], "y")];
        let head = raw[2].hash.clone();
        let m = build_history_model(&raw, 1, "main".into(), 3, &head, 6, &[]);
        assert_eq!(lanes_of(&m), vec![0, 0, 0]);
    }

    #[test]
    fn fork_and_merge_open_and_close_a_lane() {
        // a1 - b2 ------- e5(merge) - f6
        //        \ c3 - d4 /
        let raw = vec![
            rc("a1", &[], "init"),
            rc("b2", &["a1"], "x"),
            rc("c3", &["b2"], "feature work"),
            rc("d4", &["c3"], "more"),
            rc("e5", &["b2", "d4"], "Merge branch 'feature/z'"),
            rc("f6", &["e5"], "after"),
        ];
        let head = raw[5].hash.clone();
        let m = build_history_model(&raw, 1, "main".into(), 6, &head, 6, &[]);
        match &m {
            RepoModel::History { commits, .. } => {
                assert_eq!(lanes_of(&m), vec![0, 0, 1, 1, 0, 0]);
                assert!(commits[2].opens_lane, "c3 forks lane 1");
                assert_eq!(commits[4].closes_lane, Some(1), "merge closes lane 1");
                assert_eq!(
                    commits[2].fork_name.as_deref(),
                    Some("feature/z"),
                    "fork gets its name from the merge subject"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn overflow_evicts_least_recently_active() {
        // Trunk plus 3 concurrent branches with a 3-voice cap: the
        // third branch must evict the least recently active.
        let mut raw = vec![rc("a1", &[], "init")];
        raw.push(rc("b1", &["a1"], "br1")); // lane 1
        raw.push(rc("c1", &["a1"], "br2")); // lane 2
        raw.push(rc("d1", &["a1"], "br3")); // overflow: evict lane 1 (b1 oldest)
        let head = raw[0].hash.clone(); // trunk = just a1
        let m = build_history_model(&raw, 1, "main".into(), 4, &head, 3, &[]);
        match &m {
            RepoModel::History { commits, .. } => {
                assert_eq!(commits[1].lane, 1);
                assert_eq!(commits[2].lane, 2);
                assert_eq!(commits[3].lane, 1, "reuses the evicted lane");
                assert_eq!(commits[3].evicted_lane, Some(1));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn branch_names_from_subjects() {
        assert_eq!(branch_from_subject("Merge branch 'fix/bug'").as_deref(), Some("fix/bug"));
        assert_eq!(
            branch_from_subject("Merge pull request #7 from alice/feat-x").as_deref(),
            Some("feat-x")
        );
        assert_eq!(branch_from_subject("regular commit"), None);
    }
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
