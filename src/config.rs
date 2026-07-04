//! Repo-committed configuration: `.gitfugue.toml` at the repo root
//! pins scale and BPM (spec §7). Repos choose their own sound, and it
//! ships with the code. CLI flags still win over the file.

use std::path::Path;

use anyhow::{Context, Result};

use crate::model::Scale;

#[derive(Debug, Default)]
pub struct RepoConfig {
    pub scale: Option<Scale>,
    pub bpm: Option<u16>,
}

pub fn load(repo_root: &Path) -> Result<RepoConfig> {
    let path = repo_root.join(".gitfugue.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(RepoConfig::default()),
    };
    let table: toml::Table = text
        .parse()
        .with_context(|| format!("invalid TOML in {}", path.display()))?;

    let mut cfg = RepoConfig::default();
    if let Some(v) = table.get("scale") {
        let s = v
            .as_str()
            .context(".gitfugue.toml: `scale` must be a string")?;
        cfg.scale = Some(Scale::parse(s).with_context(|| {
            format!(
                ".gitfugue.toml: unknown scale '{s}' \
                 (pentatonic | minor-pentatonic | dorian | aeolian)"
            )
        })?);
    }
    if let Some(v) = table.get("bpm") {
        let n = v
            .as_integer()
            .context(".gitfugue.toml: `bpm` must be an integer")?;
        anyhow::ensure!((30..=240).contains(&n), ".gitfugue.toml: bpm out of range 30-240");
        cfg.bpm = Some(n as u16);
    }
    if table.get("palette").is_some() {
        eprintln!("gitfugue: note: .gitfugue.toml `palette` is not supported yet, ignoring");
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_cfg(dir: &Path, content: &str) {
        std::fs::write(dir.join(".gitfugue.toml"), content).unwrap();
    }

    #[test]
    fn missing_file_is_default() {
        let tmp = std::env::temp_dir().join("gitfugue-cfg-none");
        std::fs::create_dir_all(&tmp).unwrap();
        let _ = std::fs::remove_file(tmp.join(".gitfugue.toml"));
        let c = load(&tmp).unwrap();
        assert!(c.scale.is_none() && c.bpm.is_none());
    }

    #[test]
    fn parses_scale_and_bpm() {
        let tmp = std::env::temp_dir().join("gitfugue-cfg-ok");
        std::fs::create_dir_all(&tmp).unwrap();
        write_cfg(&tmp, "scale = \"dorian\"\nbpm = 92\n");
        let c = load(&tmp).unwrap();
        assert_eq!(c.scale, Some(Scale::Dorian));
        assert_eq!(c.bpm, Some(92));
    }

    #[test]
    fn rejects_bad_values() {
        let tmp = std::env::temp_dir().join("gitfugue-cfg-bad");
        std::fs::create_dir_all(&tmp).unwrap();
        write_cfg(&tmp, "scale = \"phrygian-dominant\"\n");
        assert!(load(&tmp).is_err());
        write_cfg(&tmp, "bpm = 999\n");
        assert!(load(&tmp).is_err());
    }
}
