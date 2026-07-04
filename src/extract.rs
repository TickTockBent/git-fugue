//! Extraction stage: raw facts out of git.
//!
//! Everything goes through the `Extractor` trait so the shell-out
//! implementation can be swapped for gix later without touching the
//! rest of the pipeline (spec §8).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// A tracked blob at HEAD: path plus content.
pub struct RawFile {
    pub path: PathBuf,
    pub content: Vec<u8>,
}

/// One commit from the first-parent log, oldest-first.
pub struct RawCommit {
    pub hash: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    /// insertions + deletions across the commit's diff
    pub diff_magnitude: u32,
    pub is_merge: bool,
}

pub trait Extractor {
    /// Hash of the HEAD tree (static-mode seed source).
    fn head_tree_hash(&self) -> Result<String>;
    /// All tracked files at HEAD, sorted by path, with blob contents.
    fn files_at_head(&self) -> Result<Vec<RawFile>>;
    /// Current branch name ("HEAD" when detached).
    fn head_branch(&self) -> Result<String>;
    /// Root commit reached by first-parent walk (history-mode identity seed).
    fn root_commit_hash(&self) -> Result<String>;
    /// Total first-parent commit count on HEAD.
    fn first_parent_count(&self) -> Result<u32>;
    /// First-parent log, chronological (oldest first). `limit` keeps
    /// the newest N; `range` is a raw git revision range like "A..B".
    fn first_parent_log(&self, range: Option<&str>, limit: Option<usize>)
        -> Result<Vec<RawCommit>>;
}

/// Day-one implementation: shell out to the git binary.
pub struct ShellGit {
    repo: PathBuf,
}

impl ShellGit {
    pub fn open(path: &Path) -> Result<Self> {
        let out = git(path, &["rev-parse", "--show-toplevel"])
            .context("not a git repository (or git not installed)")?;
        let top = String::from_utf8(out)?.trim().to_string();
        if top.is_empty() {
            bail!("not a git repository: {}", path.display());
        }
        Ok(ShellGit { repo: PathBuf::from(top) })
    }
}

impl Extractor for ShellGit {
    fn head_tree_hash(&self) -> Result<String> {
        let out = git(&self.repo, &["rev-parse", "HEAD^{tree}"])
            .context("repository has no commits yet (no HEAD)")?;
        Ok(String::from_utf8(out)?.trim().to_string())
    }

    fn files_at_head(&self) -> Result<Vec<RawFile>> {
        // One process for the listing, one for all blob contents.
        // Reads exactly HEAD, so a dirty working tree cannot change the song.
        let listing = git(&self.repo, &["ls-tree", "-r", "-z", "HEAD"])?;
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        for record in listing.split(|b| *b == 0) {
            if record.is_empty() {
                continue;
            }
            // Format: "<mode> <type> <oid>\t<path>"
            let tab = match record.iter().position(|b| *b == b'\t') {
                Some(i) => i,
                None => continue,
            };
            let meta = std::str::from_utf8(&record[..tab])?;
            let path = String::from_utf8_lossy(&record[tab + 1..]).into_owned();
            let mut parts = meta.split_whitespace();
            let _mode = parts.next();
            let typ = parts.next().unwrap_or("");
            let oid = parts.next().unwrap_or("");
            if typ != "blob" {
                continue; // skip submodules etc.
            }
            entries.push((oid.to_string(), PathBuf::from(path)));
        }
        entries.sort_by(|a, b| a.1.cmp(&b.1));

        // Batch-read every blob in a single git process.
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn git cat-file")?;
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().unwrap();
            for (oid, _) in &entries {
                writeln!(stdin, "{oid}")?;
            }
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("git cat-file --batch failed");
        }

        let mut files = Vec::with_capacity(entries.len());
        let mut cursor = &out.stdout[..];
        for (oid, path) in entries {
            // Header line: "<oid> <type> <size>\n"
            let nl = cursor
                .iter()
                .position(|b| *b == b'\n')
                .context("truncated cat-file output")?;
            let header = std::str::from_utf8(&cursor[..nl])?;
            let mut parts = header.split_whitespace();
            let got_oid = parts.next().unwrap_or("");
            let size: usize = parts.next_back().unwrap_or("0").parse().unwrap_or(0);
            if !got_oid.starts_with(&oid) && got_oid != oid {
                bail!("cat-file oid mismatch: wanted {oid}, got {got_oid}");
            }
            let start = nl + 1;
            let content = cursor[start..start + size].to_vec();
            cursor = &cursor[start + size + 1..]; // +1 for trailing newline
            files.push(RawFile { path, content });
        }
        Ok(files)
    }

    fn head_branch(&self) -> Result<String> {
        let out = git(&self.repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok(String::from_utf8(out)?.trim().to_string())
    }

    fn root_commit_hash(&self) -> Result<String> {
        let out = git(
            &self.repo,
            &["rev-list", "--max-parents=0", "--first-parent", "HEAD"],
        )
        .context("repository has no commits yet (no HEAD)")?;
        let text = String::from_utf8(out)?;
        text.lines()
            .last()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .context("could not find a root commit")
    }

    fn first_parent_count(&self) -> Result<u32> {
        let out = git(&self.repo, &["rev-list", "--count", "--first-parent", "HEAD"])?;
        Ok(String::from_utf8(out)?.trim().parse().unwrap_or(0))
    }

    fn first_parent_log(
        &self,
        range: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<RawCommit>> {
        // Records separated by \x01, header fields by \x1f, then
        // numstat lines until the next record.
        let mut args: Vec<String> = vec![
            "log".into(),
            "--first-parent".into(),
            "--numstat".into(),
            "--format=%x01%H%x1f%P%x1f%an%x1f%ae%x1f%at".into(),
        ];
        if let Some(n) = limit {
            args.push("-n".into());
            args.push(n.to_string());
        }
        args.push(range.unwrap_or("HEAD").to_string());
        let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = git(&self.repo, &argrefs).context("git log failed (bad --range?)")?;
        let text = String::from_utf8_lossy(&out);

        let mut commits = Vec::new();
        for record in text.split('\u{01}').skip(1) {
            let mut lines = record.lines();
            let header = match lines.next() {
                Some(h) => h,
                None => continue,
            };
            let fields: Vec<&str> = header.split('\u{1f}').collect();
            if fields.len() < 5 {
                continue;
            }
            let mut magnitude: u64 = 0;
            for line in lines {
                let mut cols = line.split('\t');
                let ins = cols.next().unwrap_or("").trim();
                let del = cols.next().unwrap_or("").trim();
                // Binary files show "-"; count them as a small fixed cost.
                magnitude += ins.parse::<u64>().unwrap_or(if ins == "-" { 8 } else { 0 });
                magnitude += del.parse::<u64>().unwrap_or(if del == "-" { 8 } else { 0 });
            }
            commits.push(RawCommit {
                hash: fields[0].trim().to_string(),
                is_merge: fields[1].split_whitespace().count() > 1,
                author_name: fields[2].trim().to_string(),
                author_email: fields[3].trim().to_ascii_lowercase(),
                timestamp: fields[4].trim().parse().unwrap_or(0),
                diff_magnitude: magnitude.min(u32::MAX as u64) as u32,
            });
        }
        commits.reverse(); // git log is newest-first; the score reads oldest-first
        Ok(commits)
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(out.stdout)
}
