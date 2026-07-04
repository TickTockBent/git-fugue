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
const GOLDEN_HISTORY: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden_history.mid");

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    sh_env(dir, cmd, args, &[]);
}

fn sh_env(dir: &Path, cmd: &str, args: &[&str], env: &[(&str, &str)]) {
    let mut c = Command::new(cmd);
    c.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c
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

/// History fixture: every commit pins author, email, and both dates,
/// so commit hashes -- and therefore the rendered bytes -- are stable.
/// Two humans, one bot, a merge, and a 60-day gap (breath bar).
fn build_history_fixture(dir: &Path) {
    sh(dir, "git", &["init", "-q", "-b", "main"]);
    sh(dir, "git", &["config", "user.email", "fixture@gitfugue.test"]);
    sh(dir, "git", &["config", "user.name", "Fixture"]);

    let day = 86_400i64;
    let t0 = 1_600_000_000i64;
    let mut commit = |file: &str, content: &str, msg: &str, author: (&str, &str), ts: i64| {
        std::fs::write(dir.join(file), content).unwrap();
        sh(dir, "git", &["add", "-A"]);
        let date = format!("{ts} +0000");
        sh_env(
            dir,
            "git",
            &["commit", "-q", "-m", msg],
            &[
                ("GIT_AUTHOR_NAME", author.0),
                ("GIT_AUTHOR_EMAIL", author.1),
                ("GIT_AUTHOR_DATE", &date),
                ("GIT_COMMITTER_NAME", "Fixture"),
                ("GIT_COMMITTER_EMAIL", "fixture@gitfugue.test"),
                ("GIT_COMMITTER_DATE", &date),
            ],
        );
    };

    let alice = ("Alice", "alice@example.com");
    let bob = ("Bob", "bob@example.com");
    let bot = ("dependabot[bot]", "49699333+dependabot[bot]@users.noreply.github.com");

    commit("main.py", "def main():\n    pass\n", "init", alice, t0);
    commit("main.py", "def main():\n    run()\n\ndef run():\n    pass\n", "add run", alice, t0 + day);
    commit("util.py", "def helper(x):\n    return x * 2\n", "helpers", bob, t0 + 2 * day);
    commit("deps.txt", "requests==2.28.0\n", "bump requests", bot, t0 + 3 * day);
    commit("main.py", "def main():\n    run()\n    log()\n\ndef run():\n    pass\n\ndef log():\n    print('hi')\n", "logging", alice, t0 + 4 * day);
    // Side branch merged back: the first-parent walk sees a merge commit.
    sh(dir, "git", &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    commit("feature.py", "def feat():\n    return 1\n", "feature work", bob, t0 + 5 * day);
    sh(dir, "git", &["checkout", "-q", "main"]);
    let mdate = format!("{} +0000", t0 + 6 * day);
    sh_env(
        dir,
        "git",
        &["merge", "-q", "--no-ff", "-m", "Merge branch 'feature'", "feature"],
        &[
            ("GIT_AUTHOR_NAME", "Alice"),
            ("GIT_AUTHOR_EMAIL", "alice@example.com"),
            ("GIT_AUTHOR_DATE", &mdate),
            ("GIT_COMMITTER_NAME", "Fixture"),
            ("GIT_COMMITTER_EMAIL", "fixture@gitfugue.test"),
            ("GIT_COMMITTER_DATE", &mdate),
        ],
    );
    // A long quiet stretch, then one more change: exercises the breath.
    commit("util.py", "def helper(x):\n    return x * 3\n", "tune helper", bob, t0 + 66 * day);

    // A branch that edits the same line main just changed: merging it
    // conflicts, exercising the merge-tree suspension bar.
    sh(dir, "git", &["checkout", "-q", "-b", "hotfix", "HEAD~1"]);
    commit("util.py", "def helper(x):\n    return x * 9\n", "hotfix helper", bob, t0 + 67 * day);
    sh(dir, "git", &["checkout", "-q", "main"]);
    let cdate = format!("{} +0000", t0 + 68 * day);
    // The merge itself fails on the conflict; resolve and commit.
    let _ = Command::new("git")
        .current_dir(dir)
        .args(["merge", "-q", "--no-ff", "-m", "Merge branch 'hotfix'", "hotfix"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    std::fs::write(dir.join("util.py"), "def helper(x):\n    return x * 27\n").unwrap();
    sh(dir, "git", &["add", "-A"]);
    sh_env(
        dir,
        "git",
        &["commit", "-q", "-m", "Merge branch 'hotfix'"],
        &[
            ("GIT_AUTHOR_NAME", "Alice"),
            ("GIT_AUTHOR_EMAIL", "alice@example.com"),
            ("GIT_AUTHOR_DATE", &cdate),
            ("GIT_COMMITTER_NAME", "Fixture"),
            ("GIT_COMMITTER_EMAIL", "fixture@gitfugue.test"),
            ("GIT_COMMITTER_DATE", &cdate),
        ],
    );
}

fn render_history(repo: &Path, out: &Path) -> Vec<u8> {
    let exe = env!("CARGO_BIN_EXE_gitfugue");
    let status = Command::new(exe)
        .args(["history"])
        .arg(repo)
        .arg("-o")
        .arg(out)
        .status()
        .expect("failed to run gitfugue");
    assert!(status.success(), "gitfugue history exited nonzero");
    std::fs::read(out).expect("output file missing")
}

#[test]
fn golden_history_midi_is_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("histfix");
    std::fs::create_dir(&repo).unwrap();
    build_history_fixture(&repo);

    let a = render_history(&repo, &tmp.path().join("a.mid"));
    let b = render_history(&repo, &tmp.path().join("b.mid"));
    assert_eq!(a, b, "two renders of the same history differ");

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(GOLDEN_HISTORY, &a).unwrap();
        eprintln!("golden file updated: {GOLDEN_HISTORY}");
        return;
    }
    let golden = std::fs::read(GOLDEN_HISTORY)
        .expect("tests/fixtures/golden_history.mid missing; run with UPDATE_GOLDEN=1");
    assert_eq!(
        a, golden,
        "history output no longer matches golden fixture; if the engine \
         changed intentionally, bump the version and regenerate with UPDATE_GOLDEN=1"
    );
}

#[test]
fn history_liner_notes_name_the_players() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("histfix");
    std::fs::create_dir(&repo).unwrap();
    build_history_fixture(&repo);

    let exe = env!("CARGO_BIN_EXE_gitfugue");
    let out = Command::new(exe)
        .args(["history"])
        .arg(&repo)
        .args(["--verbose", "-o"])
        .arg(tmp.path().join("v.mid"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("alice@example.com ->"), "missing alice voice line:\n{text}");
    assert!(text.contains("dependabot[bot] -> hi-hat"), "missing bot percussion line:\n{text}");
    assert!(text.contains("enters (feature"), "missing fugal entry line:\n{text}");
    assert!(text.contains("merge feature"), "missing merge event line:\n{text}");
    assert!(text.contains("breath"), "missing breath (gap) line:\n{text}");
    assert!(text.contains("conflict"), "missing conflict tension line:\n{text}");
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
