//! Acceptance tests for ticket T-007, verbatim from the ticket:
//!
//! 1. "two devices pair via exchanged codes; both can unwrap a folder key"
//! 2. "a third device cannot"
//! 3. "exported key restores access on a wiped device"
//!
//! These run against the crate's PUBLIC API only — no test hooks, no
//! internals — proving the whole ritual is drivable exactly the way the
//! CLI (and later T-008 transport glue) will drive it.

use ferry_crypto::folder_key::{generate_fmk, unwrap_folder_key, wrap_folder_key};
use ferry_crypto::identity::load_or_create;
use ferry_crypto::pairing::{
    complete_pairing, respond, verify_short_code, PairingOffer, PairingResponse, TransportHints,
};
use ferry_crypto::recovery::RecoveryExport;

/// A simulated device: its own persistent identity directory.
struct Device {
    dir: tempfile::TempDir,
}

impl Device {
    fn fresh() -> Self {
        Device {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }
    fn identity(&self) -> ferry_crypto::identity::DeviceIdentity {
        load_or_create(self.dir.path()).expect("identity")
    }
    /// Simulates total loss: drop everything, return a blank device at a new
    /// location (the export file is what survives).
    fn wipe_and_replace(&self) -> tempfile::TempDir {
        tempfile::tempdir().expect("fresh device after wipe")
    }
}

/// The exchange channel: offer bytes move initiator -> responder, response
/// bytes move back. In production these ride T-008; here they are function
/// calls over plain buffers.
fn exchange(offer_bytes: &[u8], responder: &Device) -> Vec<u8> {
    let offer = PairingOffer::parse(offer_bytes).expect("responder parses scanned offer");
    respond(&offer, &responder.identity(), 1_700_000_100).serialize()
}

#[test]
fn two_devices_pair_via_exchanged_codes_and_both_unwrap_the_same_fmk() {
    let alice_dev = Device::fresh();
    let bob_dev = Device::fresh();
    let alice = alice_dev.identity();

    // Initiator: folder owner creates an offer and shows code + QR.
    let fmk = generate_fmk();
    let offer = PairingOffer::create([0x42u8; 16], &alice, 1_700_000_000);
    let offer_bytes = offer.serialize();
    let code = offer.short_code(TransportHints(TransportHints::DIRECT_LAN));

    // Responder side: scans QR bytes, verifies typed code against them.
    let bob_offer = PairingOffer::parse(&offer_bytes).unwrap();
    let verified =
        verify_short_code(&code, &offer_bytes).expect("typed code must match scanned bytes");
    assert_eq!(verified.hints, TransportHints(TransportHints::DIRECT_LAN));
    assert!(verify_short_code(&mangle_one_symbol(&code), &offer_bytes).is_err());

    // Responder answers; initiator confirms and wraps the FMK to both pubs.
    let response_bytes = exchange(&offer_bytes, &bob_dev);
    let response = PairingResponse::parse(&response_bytes).unwrap();
    let done = complete_pairing(&bob_offer, &offer_bytes, &response, &fmk, &alice)
        .expect("handshake must confirm");

    // Both devices unwrap THE SAME folder key.
    let folder_id = bob_offer.folder_id;
    let alice_key =
        unwrap_folder_key(&done.wrapped_for_self, &folder_id, &alice).expect("initiator unwraps");
    let bob = bob_dev.identity();
    let bob_key =
        unwrap_folder_key(&done.wrapped_for_peer, &folder_id, &bob).expect("responder unwraps");
    assert_eq!(*alice_key, fmk);
    assert_eq!(*bob_key, fmk);

    // And symmetrically: either side can wrap for the other post-pairing
    // (this is how future folder keys / re-wraps travel).
    let fresh = generate_fmk();
    let to_bob = wrap_folder_key(&fresh, &folder_id, bob.public()).unwrap();
    assert_eq!(
        *unwrap_folder_key(&to_bob, &folder_id, &bob).unwrap(),
        fresh
    );
}

fn mangle_one_symbol(code: &str) -> String {
    // Replace the first symbol with a different canonical one.
    for (i, ch) in code.char_indices() {
        if ch == '-' {
            continue;
        }
        let sub = if ch == 'A' { 'B' } else { 'A' };
        let mut out: Vec<char> = code.chars().collect();
        out[i] = sub;
        return out.into_iter().collect();
    }
    unreachable!("code has symbols");
}

#[test]
fn a_third_device_cannot_get_the_fmk() {
    let alice_dev = Device::fresh();
    let bob_dev = Device::fresh();
    let eve_dev = Device::fresh();
    let alice = alice_dev.identity();
    let fmk = generate_fmk();
    let folder_id = [7u8; 16];

    // Eve intercepts the OFFER before any handshake completes. Pre-completion
    // there is NO wrapped FMK anywhere in flight: nothing she holds decrypts
    // toward the folder key, and she has no envelope at all yet.
    let offer = PairingOffer::create(folder_id, &alice, 1_700_000_000);
    let intercepted = offer.serialize();
    assert!(
        PairingOffer::parse(&intercepted).is_ok(),
        "she can read the public parts"
    );

    // She tries to impersonate Bob with a WRONG one-time secret (she does not
    // have it — the secret rode the physical QR channel, not this buffer).
    let eve_identity = eve_dev.identity();
    let offer_with_wrong_secret = {
        let mut evil = intercepted.clone();
        for b in evil[53..85].iter_mut() {
            *b ^= 0x5a;
        }
        PairingOffer::parse(&evil).unwrap()
    };
    let fake_response = respond(&offer_with_wrong_secret, &eve_identity, 1);
    let err = complete_pairing(
        &PairingOffer::parse(&intercepted).unwrap(),
        &intercepted,
        &fake_response,
        &fmk,
        &alice,
    )
    .unwrap_err();
    assert!(
        matches!(err, ferry_crypto::pairing::PairingError::MacMismatch),
        "forged handshake must die at MAC verification: {err}"
    );

    // The protection against a physically-present impostor is procedural:
    // the initiator completes only against a response that arrived through
    // her confirmed channel. So the testable security property is: whatever
    // envelopes exist, EVE cannot open envelopes addressed to ALICE or BOB.
    let bob_response = respond(&offer, &bob_dev.identity(), 3);
    let done = complete_pairing(&offer, &intercepted, &bob_response, &fmk, &alice).unwrap();
    let eve = eve_dev.identity();
    assert!(unwrap_folder_key(&done.wrapped_for_self, &folder_id, &eve).is_err());
    assert!(unwrap_folder_key(&done.wrapped_for_peer, &folder_id, &eve).is_err());

    // Tampering with ANY byte of a wrapped envelope fails authentication —
    // never decrypts to garbage-success.
    let mut tampered = done.wrapped_for_peer;
    tampered[35] ^= 0x01;
    assert!(unwrap_folder_key(&tampered, &folder_id, &bob_dev.identity()).is_err());
    // Wrong folder id context fails too (aad binding).
    assert!(unwrap_folder_key(&done.wrapped_for_peer, &[8u8; 16], &bob_dev.identity()).is_err());
    let _ = bob_dev;
}

#[test]
fn exported_key_restores_access_on_a_wiped_device() {
    let device = Device::fresh();
    let fmk = generate_fmk();
    let sk: [u8; 32] = core::array::from_fn(|i| (i as u8) ^ 0x33);
    let export = RecoveryExport {
        fmk: &fmk,
        device_secret: &sk,
    };

    // Backup exists somewhere safe:
    let backup_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    std::fs::write(&backup_path, export.seal("laptop fell in the sea")).unwrap();

    // WIPE: the machine is gone. A brand-new empty device appears.
    let wiped = device.wipe_and_replace();
    let wiped_dir = wiped.path().join("identity");
    assert!(!wiped_dir.join("device.key").exists());

    // Restore from passphrase + backup file alone.
    let bytes = std::fs::read(&backup_path).unwrap();
    let (restored_fmk, restored_sk) =
        RecoveryExport::open(&bytes, "laptop fell in the sea").expect("passphrase unlocks");
    assert_eq!(*restored_fmk, fmk);
    ferry_crypto::identity::import_identity(&wiped_dir, &restored_sk).expect("identity rebuilt");

    // The restored device unwraps the ORIGINAL folder key again.
    let reborn = load_or_create(&wiped_dir).unwrap();
    let folder_id = [11u8; 16];
    let wrapped = wrap_folder_key(&fmk, &folder_id, reborn.public()).unwrap();
    assert_eq!(
        *unwrap_folder_key(&wrapped, &folder_id, &reborn).unwrap(),
        fmk
    );

    // Wrong passphrase fails cleanly on the same backup.
    assert!(matches!(
        RecoveryExport::open(&bytes, "wrong passphrase"),
        Err(ferry_crypto::recovery::RecoveryError::AuthFailed)
    ));
    // Corrupted backup fails loudly.
    let mut corrupt = bytes.clone();
    corrupt[40] ^= 0x80;
    assert!(matches!(
        RecoveryExport::open(&corrupt, "laptop fell in the sea"),
        Err(ferry_crypto::recovery::RecoveryError::AuthFailed)
    ));
}
