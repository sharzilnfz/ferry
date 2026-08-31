use ferry_crypto::folder_key::{generate_fmk, unwrap_folder_key, wrap_folder_key};
use ferry_crypto::identity::load_or_create;
use ferry_crypto::pairing::{
    complete_pairing, respond, verify_short_code, PairingOffer, PairingResponse, TransportHints,
};
use ferry_crypto::recovery::RecoveryExport;

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

    #[allow(clippy::unused_self)]
    fn wipe_and_replace(&self) -> tempfile::TempDir {
        tempfile::tempdir().expect("fresh device after wipe")
    }
}

fn exchange(offer_bytes: &[u8], responder: &Device) -> Vec<u8> {
    let offer = PairingOffer::parse(offer_bytes).expect("responder parses scanned offer");
    respond(&offer, &responder.identity(), 1_700_000_100).serialize()
}

#[test]
fn two_devices_pair_via_exchanged_codes_and_both_unwrap_the_same_fmk() {
    let alice_dev = Device::fresh();
    let bob_dev = Device::fresh();
    let alice = alice_dev.identity();

    let fmk = generate_fmk();
    let offer = PairingOffer::create([0x42u8; 16], &alice, 1_700_000_000);
    let offer_bytes = offer.serialize();
    let code = offer.short_code(TransportHints(TransportHints::DIRECT_LAN));

    let bob_offer = PairingOffer::parse(&offer_bytes).unwrap();
    let verified =
        verify_short_code(&code, &offer_bytes).expect("typed code must match scanned bytes");
    assert_eq!(verified.hints, TransportHints(TransportHints::DIRECT_LAN));
    assert!(verify_short_code(&mangle_one_symbol(&code), &offer_bytes).is_err());

    let response_bytes = exchange(&offer_bytes, &bob_dev);
    let response = PairingResponse::parse(&response_bytes).unwrap();
    let done = complete_pairing(&bob_offer, &offer_bytes, &response, &fmk, &alice)
        .expect("handshake must confirm");

    let folder_id = bob_offer.folder_id;
    let alice_key =
        unwrap_folder_key(&done.wrapped_for_self, &folder_id, &alice).expect("initiator unwraps");
    let bob = bob_dev.identity();
    let bob_key =
        unwrap_folder_key(&done.wrapped_for_peer, &folder_id, &bob).expect("responder unwraps");
    assert_eq!(*alice_key, fmk);
    assert_eq!(*bob_key, fmk);

    let fresh = generate_fmk();
    let to_bob = wrap_folder_key(&fresh, &folder_id, bob.public()).unwrap();
    assert_eq!(
        *unwrap_folder_key(&to_bob, &folder_id, &bob).unwrap(),
        fresh
    );
}

fn mangle_one_symbol(code: &str) -> String {
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

    let offer = PairingOffer::create(folder_id, &alice, 1_700_000_000);
    let intercepted = offer.serialize();
    assert!(
        PairingOffer::parse(&intercepted).is_ok(),
        "she can read the public parts"
    );

    let eve_identity = eve_dev.identity();
    let offer_with_wrong_secret = {
        let mut evil = intercepted.clone();
        for b in &mut evil[53..85] {
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

    let bob_response = respond(&offer, &bob_dev.identity(), 3);
    let done = complete_pairing(&offer, &intercepted, &bob_response, &fmk, &alice).unwrap();
    let eve = eve_dev.identity();
    assert!(unwrap_folder_key(&done.wrapped_for_self, &folder_id, &eve).is_err());
    assert!(unwrap_folder_key(&done.wrapped_for_peer, &folder_id, &eve).is_err());

    let mut tampered = done.wrapped_for_peer;
    tampered[35] ^= 0x01;
    assert!(unwrap_folder_key(&tampered, &folder_id, &bob_dev.identity()).is_err());

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

    let backup_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    std::fs::write(&backup_path, export.seal("laptop fell in the sea")).unwrap();

    let wiped = device.wipe_and_replace();
    let wiped_dir = wiped.path().join("identity");
    assert!(!wiped_dir.join("device.key").exists());

    let bytes = std::fs::read(&backup_path).unwrap();
    let (restored_fmk, restored_sk) =
        RecoveryExport::open(&bytes, "laptop fell in the sea").expect("passphrase unlocks");
    assert_eq!(*restored_fmk, fmk);
    ferry_crypto::identity::import_identity(&wiped_dir, &restored_sk).expect("identity rebuilt");

    let reborn = load_or_create(&wiped_dir).unwrap();
    let folder_id = [11u8; 16];
    let wrapped = wrap_folder_key(&fmk, &folder_id, reborn.public()).unwrap();
    assert_eq!(
        *unwrap_folder_key(&wrapped, &folder_id, &reborn).unwrap(),
        fmk
    );

    assert!(matches!(
        RecoveryExport::open(&bytes, "wrong passphrase"),
        Err(ferry_crypto::recovery::RecoveryError::AuthFailed)
    ));

    let mut corrupt = bytes.clone();
    corrupt[40] ^= 0x80;
    assert!(matches!(
        RecoveryExport::open(&corrupt, "laptop fell in the sea"),
        Err(ferry_crypto::recovery::RecoveryError::AuthFailed)
    ));
}
