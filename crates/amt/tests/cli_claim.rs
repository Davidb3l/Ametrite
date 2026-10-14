//! CLI + MCP tests for `claim --issue` semantics, lease TTL validation, and
//! the MCP stdin loop's resilience.
//!
//! `claim --issue` is the lease heartbeat Sirius Forester sends. It may renew
//! a live lease, or take a fresh claim `claim_next` would also serve; it must
//! never reopen in_review/done work or jump a dependency. A refusal is exit 0
//! with `{"claimed": false, "reason": ...}`, the shape Sirius reads as "lease
//! not held".
//!
//! Every run passes `AMT_REGISTRY` as a CHILD-process env var, so these tests
//! never touch the developer's real `~/.ametrite/registry.json`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn amt(dir: &Path, reg: &Path, agent: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(dir)
        .env("AMT_REGISTRY", reg)
        .env("AMT_AGENT", agent)
        .args(args)
        .output()
        .expect("run amt")
}

fn json(o: &Output) -> serde_json::Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("one JSON value on stdout")
}

fn workspace() -> (TempDir, PathBuf, PathBuf) {
    let root = TempDir::new().unwrap();
    let reg = root.path().join("registry.json");
    let repo = root.path().join("claim-test");
    std::fs::create_dir_all(&repo).unwrap();
    let init = amt(
        &repo,
        &reg,
        "op",
        &["init", "--name", "Claim", "--prefix", "CL"],
    );
    assert!(init.status.success());
    (root, reg, repo)
}

fn create(repo: &Path, reg: &Path, title: &str) {
    let o = amt(repo, reg, "op", &["issue", "create", "--title", title]);
    assert!(o.status.success());
}

fn status_of(repo: &Path, reg: &Path, key: &str) -> String {
    let v = json(&amt(repo, reg, "op", &["--json", "issue", "show", key]));
    v["status"].as_str().unwrap().to_string()
}

/// Assert a refused `claim --issue`: exit 0, `claimed:false`, a reason that
/// contains `needle`.
fn assert_refused(o: &Output, needle: &str) {
    let v = json(o);
    assert_eq!(v["claimed"], false, "{v}");
    let reason = v["reason"].as_str().expect("reason");
    assert!(reason.contains(needle), "{reason}");
}

#[test]
fn heartbeat_renews_a_live_lease_then_cannot_reopen_in_review_work() {
    let (_root, reg, repo) = workspace();
    create(&repo, &reg, "Work");
    let claim = || {
        amt(
            &repo,
            &reg,
            "sirius/oak",
            &["--json", "claim", "--issue", "CL-1"],
        )
    };
    // Fresh claim, then a renewal while the lease is live.
    assert_eq!(json(&claim())["status"], "in_progress");
    assert_eq!(json(&claim())["claimed_by"], "sirius/oak");
    // The gate moves it to in_review (ending the lease): the next heartbeat is
    // refused instead of flipping it back to in_progress.
    let released = amt(
        &repo,
        &reg,
        "sirius/oak",
        &["release", "CL-1", "--status", "in_review"],
    );
    assert!(released.status.success());
    assert_refused(&claim(), "in_review");
    assert_eq!(status_of(&repo, &reg, "CL-1"), "in_review");
}

#[test]
fn claim_issue_refuses_done_work_and_blocked_issues() {
    let (_root, reg, repo) = workspace();
    create(&repo, &reg, "Blocker");
    create(&repo, &reg, "Blocked");
    create(&repo, &reg, "Finished");
    assert!(amt(&repo, &reg, "op", &["dep", "add", "CL-1", "CL-2"])
        .status
        .success());
    let done = amt(
        &repo,
        &reg,
        "op",
        &["issue", "update", "CL-3", "--status", "done"],
    );
    assert!(done.status.success());

    let blocked = amt(&repo, &reg, "w", &["--json", "claim", "--issue", "CL-2"]);
    assert_refused(&blocked, "open blocker");
    assert_eq!(status_of(&repo, &reg, "CL-2"), "backlog");

    let done = amt(&repo, &reg, "w", &["--json", "claim", "--issue", "CL-3"]);
    assert_refused(&done, "done");
    assert_eq!(status_of(&repo, &reg, "CL-3"), "done");

    // Human output refuses the same way, still exit 0.
    let human = amt(&repo, &reg, "w", &["claim", "--issue", "CL-3"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).starts_with("not claimed:"));
}

#[test]
fn claim_issue_held_by_another_agent_is_a_refusal_not_an_error() {
    let (_root, reg, repo) = workspace();
    create(&repo, &reg, "Contested");
    let alice = amt(
        &repo,
        &reg,
        "alice",
        &["--json", "claim", "--issue", "CL-1"],
    );
    assert_eq!(json(&alice)["claimed_by"], "alice");
    let bob = amt(&repo, &reg, "bob", &["--json", "claim", "--issue", "CL-1"]);
    assert_refused(&bob, "claimed by alice");
}

#[test]
fn claim_rejects_a_ttl_below_one_second() {
    let (_root, reg, repo) = workspace();
    create(&repo, &reg, "Work");
    for args in [
        &["claim", "--ttl", "0"][..],
        &["claim", "--ttl=-5"][..],
        &["claim", "--issue", "CL-1", "--ttl", "0"][..],
    ] {
        let o = amt(&repo, &reg, "w", args);
        assert!(!o.status.success(), "{args:?} should fail");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("at least 1 second"),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    assert_eq!(status_of(&repo, &reg, "CL-1"), "backlog");
    // A positive TTL still works.
    let ok = amt(&repo, &reg, "w", &["claim", "--ttl", "1"]);
    assert!(ok.status.success());
}

/// Run `amt mcp` on `input` (raw bytes), returning each stdout line as JSON.
fn mcp(repo: &Path, reg: &Path, input: &[u8]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_amt"))
        .current_dir(repo)
        .env("AMT_REGISTRY", reg)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("JSON-RPC line"))
        .collect()
}

fn call(id: u64, tool: &str, args: serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":tool,"arguments":args}})
    )
}

fn tool_text(resp: &serde_json::Value) -> serde_json::Value {
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| text.into())
}

#[test]
fn mcp_claim_issue_refuses_with_claimed_false_and_validates_ttl() {
    let (_root, reg, repo) = workspace();
    create(&repo, &reg, "Work");
    let moved = amt(
        &repo,
        &reg,
        "a",
        &["issue", "update", "CL-1", "--status", "in_review"],
    );
    assert!(moved.status.success());
    let mut input = String::new();
    input += &call(
        1,
        "claim_issue",
        serde_json::json!({"id":"CL-1","agent":"a"}),
    );
    input += &call(
        2,
        "claim_issue",
        serde_json::json!({"id":"CL-1","agent":"a","ttl_seconds":0}),
    );
    input += &call(
        3,
        "claim_next_issue",
        serde_json::json!({"agent":"a","ttl_seconds":-1}),
    );
    let out = mcp(&repo, &reg, input.as_bytes());
    let refused = tool_text(&out[0]);
    assert_eq!(refused["claimed"], false);
    assert!(refused["reason"].as_str().unwrap().contains("in_review"));
    for resp in &out[1..3] {
        assert_eq!(resp["result"]["isError"], true, "{resp}");
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("at least 1 second"));
    }
    assert_eq!(status_of(&repo, &reg, "CL-1"), "in_review");
}

#[test]
fn mcp_survives_a_line_that_is_not_valid_utf8() {
    let (_root, reg, repo) = workspace();
    let mut input: Vec<u8> = Vec::new();
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"p\xff\xfeing\"}\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n");
    let out = mcp(&repo, &reg, &input);
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[1]["error"]["code"], -32700);
    assert!(out[1]["id"].is_null());
    assert_eq!(
        out[2]["id"], 3,
        "the session must continue after the bad line"
    );
    assert!(out[2]["result"].is_object());
}
