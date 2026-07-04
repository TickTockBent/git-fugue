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

pub trait Extractor {
    /// Hash of the HEAD tree (static-mode seed source).
    fn head_tree_hash(&self) -> Result<String>;
    /// All tracked files at HEAD, sorted by path, with blob contents.
    fn files_at_head(&self) -> Result<Vec<RawFile>>;
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
