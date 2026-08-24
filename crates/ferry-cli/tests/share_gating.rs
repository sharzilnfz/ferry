//! The share-time secret gate: LOUD by default, redacted always,
//! `--i-know` required to proceed.

mod common;

use common::Env;
use ferry_cli::commands;

#[test]
fn share_refuses_and_redacts_until_i_know() {
    let env = Env::new("share-gate");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj, "init").unwrap();

    // Opt .env back into sync so the scanner sees an included high-risk file
    // with real-looking credentials inside.
    let secret = "AKIAIOSFODNN7EXAMPLE";
    std::fs::write(
        proj.join(".env"),
        format!("AWS_ACCESS_KEY_ID={secret}\n"),
    )
    .unwrap();
    commands::ignore_cmd::run(&proj, Some("!.env"), None, false).unwrap();

    // Without --i-know: refused, structured code, redacted preview.
    let err = commands::share::run(&proj, false, 5).unwrap_err();
    assert_eq!(err.code, "secrets-found");
    assert!(!err.message.contains(secret), "never leak the secret itself");
    let detail = err.detail.expect("structured findings");
    let findings = detail.as_array().unwrap();
    assert!(!findings.is_empty());
    // First finding is the path-level class for .env itself.
    assert_eq!(findings[0]["path"], ".env");
    assert_eq!(findings[0]["class"], "env-file-included");
    // The credential inside appears as its own redacted finding.
    let aws = findings
        .iter()
        .find(|w| w["class"] == "aws-access-key")
        .expect("aws key flagged");
    let preview = aws["preview"].as_str().unwrap();
    assert!(preview.starts_with("AKIA"), "{preview}");
    assert!(!preview.contains(secret), "preview must be redacted: {preview}");

    // No offer file may exist after a refusal.
    assert!(!proj.join(".ferry/pair-offer.ferry-pair").exists());

    // With --i-know: proceeds past the gate into the pairing flow (offer
    // file written; the ritual then waits for an acceptor, which this test
    // does not provide — pair-timeout proves the gate opened).
    let err = commands::share::run(&proj, true, 1).unwrap_err();
    assert_eq!(err.code, "pair-timeout", "--i-know must proceed past the gate");
    assert!(proj.join(".ferry/pair-offer.ferry-pair").exists());
}

#[test]
fn share_clean_folder_emits_payload_without_gate() {
    let env = Env::new("share-clean");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj, "init").unwrap();
    std::fs::write(proj.join("README.md"), b"clean").unwrap();

    // A bare `share` blocks waiting for the acceptor; run the initiator in a
    // thread and stop after the offer file appears (gate already passed).
    let proj2 = proj.clone();
    let h = std::thread::spawn(move || commands::share::run(&proj2, false, 3));
    let offer = proj.join(".ferry/pair-offer.ferry-pair");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !offer.exists() {
        assert!(std::time::Instant::now() < deadline, "offer never written");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Complete the ritual from a second device so the thread finishes cleanly.
    let responder_home = tempfile::tempdir().unwrap();
    env.switch_home_to(responder_home.path());
    let target = env.work().join("device-b");
    std::fs::create_dir_all(&target).unwrap();
    let out = commands::pairing::accept(
        &ferry_cli::ensure_identity().unwrap(),
        &offer,
        Some(&target),
        15,
    )
    .expect("accept completes");
    assert_eq!(out.json["status"], "completed");

    let initiated = h.join().unwrap().expect("initiator completed");
    assert_eq!(initiated.json["command"], "share");
    assert_eq!(initiated.json["warnings_reviewed"], false);
}
