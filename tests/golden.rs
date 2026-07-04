//! Golden determinism test (spec §9): a fixture repo must render to
//! byte-identical MIDI, forever. The static-mode seed comes from the
//! HEAD *tree* hash, which depends only on content — so this fixture
//! can be rebuilt from scratch on every run and still hash the same.
//!
//! To regenerate after an intentional engine change:
//!   UPDATE_GOLDEN=1 cargo test --test golden

use std::path::Path;
use std::process::Command;

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden.mid");

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let out = Command::new(cmd)
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {cmd}: {e}"));
    assert!(
        out.status.success(),
        "{cmd} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn build_fixture(dir: &Path) {
    sh(dir, "git", &["init", "-q", "-b", "main"]);
    sh(dir, "git", &["config", "user.email", "fixture@gitfugue.test"]);
    sh(dir, "git", &["config", "user.name", "Fixture"]);

    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    let greeting = compose();\n    println!(\"{greeting}\");\n}\n\n\
         fn compose() -> String {\n    let mut s = String::new();\n    for i in 0..4 {\n        \
         if i % 2 == 0 {\n            s.push('x');\n        }\n    }\n    s\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/helper.py"),
        "def tempo(bpm):\n    return 60.0 / bpm\n\n\
         def swing(beats):\n    out = []\n    for b in beats:\n        if b > 0:\n            \
         out.append(b * 1.5)\n    return out\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("docs/README.md"),
        "# fixture\n\nA tiny polyglot repo that must always sound the same.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("build.sh"),
        "#!/bin/sh\nset -e\ncargo build\necho done\n",
    )
    .unwrap();

    sh(dir, "git", &["add", "-A"]);
    sh(dir, "git", &["commit", "-q", "-m", "fixture"]);
}

fn render(repo: &Path, out: &Path) -> Vec<u8> {
    let exe = env!("CARGO_BIN_EXE_gitfugue");
    let status = Command::new(exe)
        .args(["static"])
        .arg(repo)
        .arg("-o")
        .arg(out)
        .status()
        .expect("failed to run gitfugue");
    assert!(status.success(), "gitfugue exited nonzero");
    std::fs::read(out).expect("output file missing")
}

#[test]
fn golden_midi_is_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("fixture");
    std::fs::create_dir(&repo).unwrap();
    build_fixture(&repo);

    let a = render(&repo, &tmp.path().join("a.mid"));
    let b = render(&repo, &tmp.path().join("b.mid"));
    assert_eq!(a, b, "two renders of the same tree differ");

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(GOLDEN, &a).unwrap();
        eprintln!("golden file updated: {GOLDEN}");
        return;
    }
    let golden = std::fs::read(GOLDEN)
        .expect("tests/fixtures/golden.mid missing; run with UPDATE_GOLDEN=1 to create");
    assert_eq!(
        a, golden,
        "output no longer matches golden fixture; if the engine changed \
         intentionally, bump the version and regenerate with UPDATE_GOLDEN=1"
    );
}

#[test]
fn seed_override_changes_output() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("fixture");
    std::fs::create_dir(&repo).unwrap();
    build_fixture(&repo);

    let exe = env!("CARGO_BIN_EXE_gitfugue");
    let out1 = tmp.path().join("s1.mid");
    let out2 = tmp.path().join("s2.mid");
    for (seed, out) in [("deadbeef", &out1), ("cafed00d", &out2)] {
        let status = Command::new(exe)
            .args(["static"])
            .arg(&repo)
            .args(["--seed", seed, "-o"])
            .arg(out)
            .status()
            .unwrap();
        assert!(status.success());
    }
    assert_ne!(
        std::fs::read(&out1).unwrap(),
        std::fs::read(&out2).unwrap(),
        "different seeds must change the song"
    );
}
