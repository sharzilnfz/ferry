//! ADR-0006 short-code guarantees, exercised through the ONLY public way to
//! mint or answer a pairing code: the `PairingRitual`. The code mechanics
//! live privately inside `ferry-folder::pairing` (the old public
//! `ferry-crypto::pairing_code` module is gone); these are the cases from
//! its test suite that the ritual seam can still express. Round-trip,
//! wrong-code, expiry-consumption, and one-time-use behavior are covered by
//! `ritual.rs`; this file pins format, entropy, input normalization, and
//! the source-level crypto guarantees.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ferry_crypto::identity::{load_or_create, DeviceIdentity};
use ferry_folder::folder::{
    create_folder, open_folder, save_settings, OpenFolder, Settings, SETTINGS_FORMAT_VERSION,
};
use ferry_folder::pairing::{
    new_shared_rendezvous, PairingRitual, PendingAcceptance, SharedRendezvous,
};

const FOLDER_ID: [u8; 16] = [9u8; 16];
const POLY: u64 = 0x0FED_CBA9_8765_4321;

/// Error-code assertion helper: the ritual's result types are plain structs
/// without Debug, so `unwrap_err` is not available.
#[track_caller]
fn code_of<T>(r: Result<T, ferry_folder::FolderError>) -> &'static str {
    match r {
        Err(e) => e.code,
        Ok(_) => panic!("expected an error"),
    }
}

/// Keeps the identity tempdir alive for the test's duration.
struct Home(tempfile::TempDir);

impl Home {
    fn path(&self) -> &Path {
        self.0.path()
    }
}

fn identity(tag: &str) -> (Home, DeviceIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let id = load_or_create(&dir.path().join(tag)).unwrap();
    (Home(dir), id)
}

fn default_settings() -> Settings {
    Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&FOLDER_ID),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    }
}

/// Device A's side of setup: an owned, opened folder.
fn opened_owner_folder(root: &Path, id: &DeviceIdentity) -> OpenFolder {
    std::fs::create_dir_all(root).unwrap();
    let (store, _fmk) = create_folder(root, id, FOLDER_ID, POLY).unwrap();
    save_settings(root, &default_settings()).unwrap();
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    open_folder(root, id).unwrap()
}

/// One ritual per device over an ISOLATED rendezvous map.
fn ritual(home: &Path, id: &DeviceIdentity, rendezvous: &SharedRendezvous) -> PairingRitual {
    PairingRitual::with_shared(home.to_path_buf(), id.clone(), Arc::clone(rendezvous))
}

#[test]
fn codes_are_six_base32_symbols_with_matching_checksums() {
    // 25 bits of entropy over 33M possibilities: for 1000 codes the
    // birthday collision expectation is ~0.015, so >900 unique passes
    // reliably (ADR-0006). The checksum symbol must recompute from the
    // first five — the ritual mints exactly the ADR-0006 format.
    let work = tempfile::tempdir().unwrap();
    let (home_a, id_a) = identity("a");
    let opened_a = opened_owner_folder(&work.path().join("device-a"), &id_a);
    let rendezvous = new_shared_rendezvous();
    let ritual_a = ritual(home_a.path(), &id_a, &rendezvous);

    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let pending = ritual_a.create_offer(&opened_a).unwrap();
        let code = pending.short_code.clone();
        assert_eq!(code.len(), 6, "{code}");
        assert!(
            code.chars()
                .all(|c| ferry_crypto::base32::ALPHABET.contains(&(c as u8))),
            "{code}"
        );
        let (data, checksum) = code.split_at(5);
        let expected = ferry_crypto::base32::ALPHABET
            [(ferry_crypto::crc32::crc32(data.as_bytes()) % 32) as usize];
        assert_eq!(checksum.as_bytes()[0], expected, "{code}");
        seen.insert(code.clone());
        // Best-effort cleanup of the one-time rendezvous file (same
        // documented `/tmp/ferry-rendezvous-<CODE>.json` naming).
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("ferry-rendezvous-{code}.json")),
        );
    }
    assert!(seen.len() > 900, "got {}", seen.len());
}

#[test]
fn typed_codes_tolerate_case_and_separators() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    let code = &pending_i.short_code;

    // A human types what they see: lowercase and hyphenated variants of
    // the rendered code must dial the same session.
    let lowered = code.to_ascii_lowercase();
    let pending_b = accept_code_pending(&lowered, home_b.path(), &id_b, &rendezvous, &target_b);
    assert_eq!(pending_b.complete(0).unwrap().folder_id, FOLDER_ID);

    // Fresh offer for the hyphenated form (sessions are one-time), into a
    // fresh target (adoption refuses an initialized directory).
    let pending_i2 = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    let s = pending_i2.short_code.clone();
    let hyphenated = format!("{}-{}", &s[..3], &s[3..]);
    let pending_b2 = accept_code_pending(
        &hyphenated,
        home_b.path(),
        &id_b,
        &rendezvous,
        &work.path().join("device-b-2"),
    );
    assert_eq!(pending_b2.complete(0).unwrap().folder_id, FOLDER_ID);
}

fn accept_code_pending(
    code: &str,
    home_b: &Path,
    id_b: &DeviceIdentity,
    rendezvous: &SharedRendezvous,
    target_b: &Path,
) -> PendingAcceptance {
    ritual(home_b, id_b, rendezvous)
        .accept_offer(code, Some(target_b))
        .unwrap()
}

#[test]
fn checksum_symbol_flip_is_refused() {
    // The sixth symbol is a checksum: corrupting it while the data
    // symbols stay intact must not dial any session.
    let work = tempfile::tempdir().unwrap();
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&work.path().join("device-a"), &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    let code = pending_i.short_code.clone();

    let mut flipped: Vec<char> = code.chars().collect();
    let last = flipped[5];
    flipped[5] = if last == 'A' { 'B' } else { 'A' };
    let flipped: String = flipped.into_iter().collect();
    assert_ne!(flipped, code);

    let err =
        code_of(ritual(home_b.path(), &id_b, &rendezvous).accept_offer(&flipped, Some(&target_b)));
    assert_eq!(err, "pairing-not-found");
}

#[test]
fn offer_expires_twenty_four_hours_out() {
    let work = tempfile::tempdir().unwrap();
    let (home_a, id_a) = identity("a");
    let opened_a = opened_owner_folder(&work.path().join("device-a"), &id_a);
    let before = SystemTime::now();
    let pending = ritual(home_a.path(), &id_a, &new_shared_rendezvous())
        .create_offer(&opened_a)
        .unwrap();
    let ttl = pending.expires_at.duration_since(before).unwrap();
    assert!(
        ttl > Duration::from_hours(24) - Duration::from_secs(60)
            && ttl < Duration::from_hours(24) + Duration::from_secs(60),
        "expected ~24h TTL, got {ttl:?}"
    );
}

#[test]
fn code_verification_is_constant_time_and_zeroized() {
    // Ported from the old ferry-crypto source scan: the verify section
    // must compare through `subtle::ct_eq` (never `==`, which leaks prefix
    // match length) and hold the code in `Zeroizing`.
    let content = std::fs::read_to_string("src/pairing.rs").unwrap_or_else(|_| {
        std::fs::read_to_string("crates/ferry-folder/src/pairing.rs").expect("pairing.rs not found")
    });
    let verify_section = content
        .find("fn verify")
        .map_or(&content[..], |p| &content[p..]);
    let verify_end = verify_section
        .find("\n    }")
        .map_or(verify_section, |p| &verify_section[..p]);
    assert!(verify_end.contains("ct_eq"), "verify should use ct_eq");
    let eq_count = verify_end.matches("==").count();
    assert_eq!(
        eq_count, 0,
        "verify should not contain ==, found {eq_count} occurrences"
    );
    assert!(content.contains("Zeroizing"));
    assert!(
        !verify_end.contains("SystemTime::now"),
        "verify must not read the clock; expiry is passed in"
    );
}
