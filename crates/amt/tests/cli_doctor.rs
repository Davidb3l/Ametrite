//! `amt doctor --json`: the handshake envelope's `ok` is the fold over GATING
//! checks. Stale claims and unresolved links are normal fleet states, so they
//! show up in `checks` (with `"gating": false`) without failing `ok`.
//!
//! Every run passes `AMT_REGISTRY` as a CHILD-process env var, so these tests
//! never touch the developer's real `~/.ametrite/registry.json`.

use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn amt(dir: &Path, reg: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(dir)
        .env("AMT_REGISTRY", reg)
        .env("AMT_AGENT", "doc")
        .args(args)
        .output()
        .expect("run amt")
}

fn json(o: &Output) -> serde_json::Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("one JSON value on stdout")
}

#[test]
fn doctor_ok_survives_stale_claims_and_unresolved_links() {
    let root = TempDir::new().unwrap();
    let reg = root.path().join("registry.json");
    let repo = root.path().join("doctor-test");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(
        amt(&repo, &reg, &["init", "--name", "Doc", "--prefix", "DR"])
            .status
            .success()
    );
    let create = |title: &str, body: &str| {
        let o = amt(
            &repo,
            &reg,
            &["issue", "create", "--title", title, "--body", body],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    create("Linky", "See [[Not Written Yet]]");
    create("Stalled", "");
    // A lease that expired a second ago (the lowest TTL the CLI accepts, then wait).
    let claim = amt(&repo, &reg, &["claim", "--issue", "DR-2", "--ttl", "1"]);
    assert!(claim.status.success());
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let env = json(&amt(&repo, &reg, &["doctor", "--json"]));
    assert_eq!(env["schemaVersion"], 1);
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["report"]["ok"], true);
    let checks = env["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 8);
    for c in checks {
        let gating = !matches!(
            c["name"].as_str().unwrap(),
            "stale_claims" | "unresolved_links"
        );
        assert_eq!(c["gating"], gating, "{c}");
    }
    let by = |name: &str| checks.iter().find(|c| c["name"] == name).unwrap();
    assert_eq!(by("stale_claims")["ok"], false);
    assert_eq!(by("unresolved_links")["ok"], false);

    // The human form still lists them rather than claiming a clean bill.
    let human = amt(&repo, &reg, &["doctor"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("stale claim: DR-2"), "{text}");
    assert!(text.contains("unresolved link"), "{text}");
    assert!(!text.contains("workspace healthy"), "{text}");
}
