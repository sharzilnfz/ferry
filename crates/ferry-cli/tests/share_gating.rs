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
    std::fs::write(proj.join(".env"), format!("AWS_ACCESS_KEY_ID={secret}\n")).unwrap();
    commands::ignore_cmd::run(&proj, Some("!.env"), None, false).unwrap();

    // Without --i-know: refused, structured code, redacted preview.
    let err = commands::share::run(&proj, false, 5).unwrap_err();
    assert_eq!(err.code, "secrets-found");
    assert!(
        !err.message.contains(secret),
        "never leak the secret itself"
    );
    let detail = err.detail.expect("structured findings");
    let findings = detail["warnings"].as_array().unwrap();
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
    assert!(
        !preview.contains(secret),
        "preview must be redacted: {preview}"
    );

    // No offer file may exist after a refusal.
    assert!(!proj.join(".ferry/pair-offer.ferry-pair").exists());

    // With --i-know: proceeds past the gate into the pairing flow (offer
    // file written and in-band session advertised).
    let ok = commands::share::run(&proj, true, 0).expect("--i-know must proceed past the gate");
    assert_eq!(ok.json["command"], "share");
    assert_eq!(ok.json["warnings_reviewed"], true);
    assert!(proj.join(".ferry/pair-offer.ferry-pair").exists());
}

#[test]
fn share_clean_folder_emits_payload_without_gate() {
    let env = Env::new("share-clean");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj, "init").unwrap();
    std::fs::write(proj.join("README.md"), b"clean").unwrap();

    let ok = commands::share::run(&proj, false, 0).expect("clean folder emits payload");
    assert_eq!(ok.json["command"], "share");
    assert_eq!(ok.json["status"], "advertising");
    assert_eq!(ok.json["warnings_reviewed"], false);
    let code = ok.json["code"].as_str().expect("emits 6-word code");
    assert_eq!(code.split('-').count(), 6);
    assert!(proj.join(".ferry/pair-offer.ferry-pair").exists());
}
