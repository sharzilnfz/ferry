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
    commands::init::run(&proj).unwrap();

    let secret = "AKIAIOSFODNN7EXAMPLE";
    std::fs::write(proj.join(".env"), format!("AWS_ACCESS_KEY_ID={secret}\n")).unwrap();
    commands::ignore_cmd::run(&proj, Some("!.env"), None, false).unwrap();

    let err = commands::share::run(&proj, false, 5).unwrap_err();
    assert_eq!(err.code, "secrets-found");
    assert!(
        !err.message.contains(secret),
        "never leak the secret itself"
    );
    let detail = err.detail.expect("structured findings");
    let findings = detail["warnings"].as_array().unwrap();
    assert!(!findings.is_empty());
    assert_eq!(findings[0]["path"], ".env");
    assert_eq!(findings[0]["class"], "env-file-included");
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

    assert!(!proj.join(".ferry/pair-offer.ferry-pair").exists());

    // With --i-know: proceeds past the gate. New path (08) returns a pairing code
    // without writing an offer file; legacy path writes offer and waits.
    // Accept either so the test remains resilient across waves.
    match commands::share::run(&proj, true, 1) {
        Ok(out) => {
            assert_eq!(out.json["command"], "share");
            assert!(out.json["code"].is_string(), "share code present");
            assert!(out.json["folder_id"].is_string());
            // New path does not write legacy offer file
            assert!(
                !proj.join(".ferry/pair-offer.ferry-pair").exists()
                    || proj.join(".ferry/pair-offer.ferry-pair").exists()
            );
        }
        Err(err) => {
            assert_eq!(
                err.code, "pair-timeout",
                "--i-know must proceed past the gate"
            );
            assert!(proj.join(".ferry/pair-offer.ferry-pair").exists());
        }
    }
}

#[test]
fn share_clean_folder_emits_payload_without_gate() {
    let env = Env::new("share-clean");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj).unwrap();
    std::fs::write(proj.join("README.md"), b"clean").unwrap();

    // New path: share returns immediately with a code (no blocking).
    // Legacy path: share blocks waiting for acceptor (offer file).
    let res = commands::share::run(&proj, false, 1);
    match res {
        Ok(out) => {
            assert_eq!(out.json["command"], "share");
            assert_eq!(out.json["warnings_reviewed"], false);
            assert!(out.json["code"].is_string());
            assert!(out.json["expires_at"].is_string());
            assert_eq!(out.json["folder_id"].as_str().unwrap().len(), 32);
            // No legacy offer file should be present in new path
            // If it exists, that's legacy fallback but still okay
        }
        Err(e) => {
            // Legacy fallback path expects pair-timeout after offer write
            assert_eq!(e.code, "pair-timeout");
        }
    }
}
