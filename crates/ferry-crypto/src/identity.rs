use rand::rngs::OsRng;
use std::fmt;
use std::path::{Path, PathBuf};
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub type DeviceId = [u8; 32];

const FILE_NAME: &str = "device.key";
const MAGIC: [u8; 4] = *b"FRID";
const FORMAT_VERSION: u8 = 1;

pub const FILE_LEN: usize = 69;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("io error touching identity store: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "device identity at {} is corrupted ({reason}); refusing to regenerate \
         because new keys would silently fork trust — restore the file from \
         backup or delete it deliberately",
        path.display()
    )]
    Corrupted { path: PathBuf, reason: &'static str },
    #[error("peer public key produced a degenerate (small-order) shared secret")]
    DegeneratePeerKey,
}

#[derive(Clone)]
pub struct DeviceIdentity {
    sk: StaticSecret,

    pk: DeviceId,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("device_id", &crate::hex_short(&self.pk))
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        Self::from_static(StaticSecret::random_from_rng(OsRng))
    }

    pub fn from_secret_bytes(secret: &[u8; 32]) -> Self {
        Self::from_static(StaticSecret::from(*secret))
    }

    fn from_static(sk: StaticSecret) -> Self {
        let mut pk: DeviceId = [0u8; 32];
        pk.copy_from_slice(PublicKey::from(&sk).as_bytes());
        DeviceIdentity { sk, pk }
    }

    pub fn device_id(&self) -> &DeviceId {
        &self.pk
    }

    pub fn public(&self) -> &DeviceId {
        &self.pk
    }

    pub fn diffie_hellman(
        &self,
        peer_public: &DeviceId,
    ) -> Result<Zeroizing<[u8; 32]>, IdentityError> {
        let shared = self.sk.diffie_hellman(&PublicKey::from(*peer_public));
        if !shared.was_contributory() {
            return Err(IdentityError::DegeneratePeerKey);
        }
        let mut out: DeviceId = [0u8; 32];
        out.copy_from_slice(shared.as_bytes());
        Ok(Zeroizing::new(out))
    }

    pub fn to_file_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FILE_LEN);
        out.extend_from_slice(&MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(self.sk.as_bytes());
        out.extend_from_slice(&self.pk);
        out
    }

    fn from_file_bytes(path: &Path, bytes: &[u8]) -> Result<Self, IdentityError> {
        let bad = |reason: &'static str| {
            Err(IdentityError::Corrupted {
                path: path.to_path_buf(),
                reason,
            })
        };
        if bytes.len() != FILE_LEN {
            return bad("wrong length");
        }
        if bytes[..4] != MAGIC {
            return bad("bad magic");
        }
        if bytes[4] != FORMAT_VERSION {
            return bad("unknown version");
        }
        let sk_bytes: [u8; 32] = bytes[5..37].try_into().expect("32 bytes");
        let stored_pk: DeviceId = bytes[37..69].try_into().expect("32 bytes");
        let candidate = Self::from_secret_bytes(&sk_bytes);
        if candidate.pk != stored_pk {
            return bad("stored public key does not match the secret key");
        }
        Ok(candidate)
    }
}

pub fn default_identity_root() -> Result<PathBuf, IdentityError> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            IdentityError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "cannot locate home directory for ~/.ferry",
            ))
        })?;
    Ok(home.join(".ferry").join("identity"))
}

pub fn load_or_create(root: &Path) -> Result<DeviceIdentity, IdentityError> {
    let file = root.join(FILE_NAME);
    match std::fs::read(&file) {
        Ok(bytes) => DeviceIdentity::from_file_bytes(&file, &bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_identity(root, &file)?;

            let bytes = std::fs::read(&file).expect("just written");
            DeviceIdentity::from_file_bytes(&file, &bytes)
        }
        Err(e) => Err(e.into()),
    }
}

fn write_identity_file(root: &Path, file: &Path, id: &DeviceIdentity) -> Result<(), IdentityError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(root)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(file)?;
        f.write_all(&id.to_file_bytes())?;
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        std::fs::create_dir_all(root)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)?;
        f.write_all(&id.to_file_bytes())?;
    }
    Ok(())
}

fn create_identity(root: &Path, file: &Path) -> Result<(), IdentityError> {
    write_identity_file(root, file, &DeviceIdentity::generate())
}

pub fn import_identity(root: &Path, secret: &[u8; 32]) -> Result<DeviceIdentity, IdentityError> {
    let file = root.join(FILE_NAME);
    match std::fs::metadata(&file) {
        Ok(_) => Err(IdentityError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "identity already exists at {}; delete it deliberately before importing",
                file.display()
            ),
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = DeviceIdentity::from_secret_bytes(secret);
            write_identity_file(root, &file, &id)?;

            let bytes = std::fs::read(&file).expect("just written");
            DeviceIdentity::from_file_bytes(&file, &bytes)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE_SK: [u8; 32] = [
        0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66,
        0x45, 0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5, 0x1d, 0xb9,
        0x2c, 0x2a,
    ];
    const ALICE_PK: [u8; 32] = [
        0x85, 0x20, 0xf0, 0x09, 0x89, 0x30, 0xa7, 0x54, 0x74, 0x8b, 0x7d, 0xdc, 0xb4, 0x3e, 0xf7,
        0x5a, 0x0d, 0xbf, 0x3a, 0x0d, 0x26, 0x38, 0x1a, 0xf4, 0xeb, 0xa4, 0xa9, 0x8e, 0xaa, 0x9b,
        0x4e, 0x6a,
    ];
    const BOB_SK: [u8; 32] = [
        0x5d, 0xab, 0x08, 0x7e, 0x62, 0x4a, 0x8a, 0x4b, 0x79, 0xe1, 0x7f, 0x8b, 0x83, 0x80, 0x0e,
        0xe6, 0x6f, 0x3b, 0xb1, 0x29, 0x26, 0x18, 0xb6, 0xfd, 0x1c, 0x2f, 0x8b, 0x27, 0xff, 0x88,
        0xe0, 0xeb,
    ];
    const BOB_PK: [u8; 32] = [
        0xde, 0x9e, 0xdb, 0x7d, 0x7b, 0x7d, 0xc1, 0xb4, 0xd3, 0x5b, 0x61, 0xc2, 0xec, 0xe4, 0x35,
        0x37, 0x3f, 0x83, 0x43, 0xc8, 0x5b, 0x78, 0x67, 0x4d, 0xad, 0xfc, 0x7e, 0x14, 0x6f, 0x88,
        0x2b, 0x4f,
    ];
    const SHARED: [u8; 32] = [
        0x4a, 0x5d, 0x9d, 0x5b, 0xa4, 0xce, 0x2d, 0xe1, 0x72, 0x8e, 0x3b, 0xf4, 0x80, 0x35, 0x0f,
        0x25, 0xe0, 0x7e, 0x21, 0xc9, 0x47, 0xd1, 0x9e, 0x33, 0x76, 0xf0, 0x9b, 0x3c, 0x1e, 0x16,
        0x17, 0x42,
    ];

    #[test]
    fn rfc7748_vectors_pin_x25519_usage() {
        let alice = DeviceIdentity::from_secret_bytes(&ALICE_SK);
        assert_eq!(alice.device_id(), &ALICE_PK);
        let bob = DeviceIdentity::from_secret_bytes(&BOB_SK);
        assert_eq!(bob.device_id(), &BOB_PK);

        assert_eq!(*alice.diffie_hellman(&BOB_PK).unwrap(), SHARED);
        assert_eq!(*bob.diffie_hellman(&ALICE_PK).unwrap(), SHARED);
    }

    #[test]
    fn generated_device_id_equals_public_key_and_file_round_trips() {
        let id = DeviceIdentity::generate();
        assert_eq!(id.device_id(), id.public());
        let parsed =
            DeviceIdentity::from_file_bytes(Path::new("<mem>"), &id.to_file_bytes()).unwrap();
        assert_eq!(parsed.device_id(), id.device_id());

        let probe = DeviceIdentity::generate();
        let a = parsed.diffie_hellman(probe.public()).unwrap();
        let b = probe.diffie_hellman(parsed.public()).unwrap();
        assert_eq!(*a, *b);
    }

    #[test]
    fn load_or_create_then_reload_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let first = load_or_create(tmp.path()).unwrap();
        let again = load_or_create(tmp.path()).unwrap();
        assert_eq!(first.device_id(), again.device_id());

        assert_eq!(first.to_file_bytes(), again.to_file_bytes());
    }

    #[test]
    fn fresh_identity_gets_restrictive_permissions_on_unix() {
        let tmp = tempfile::tempdir().unwrap();

        let root = tmp.path().join("identity");
        load_or_create(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(root.join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            let dir_mode = std::fs::metadata(&root).unwrap().permissions().mode();
            assert_eq!(file_mode & 0o777, 0o600, "identity file must be 0600");
            assert_eq!(dir_mode & 0o777, 0o700, "identity dir must be 0700");
        }
        #[cfg(not(unix))]
        {
            assert!(root.join(FILE_NAME).exists());
        }
    }

    #[test]
    fn corrupted_identity_is_a_loud_error_and_never_regenerated() {
        let tmp = tempfile::tempdir().unwrap();
        let original = load_or_create(tmp.path()).unwrap();
        let file = tmp.path().join(FILE_NAME);
        let good = std::fs::read(&file).unwrap();

        let expect_corrupt = |file: &Path| {
            let e = load_or_create(file.parent().unwrap()).unwrap_err();
            assert!(matches!(e, IdentityError::Corrupted { .. }), "{e}");
        };

        let garbage = vec![0xEE; 40];
        std::fs::write(&file, &garbage).unwrap();
        expect_corrupt(&file);
        assert_eq!(
            std::fs::read(&file).unwrap(),
            garbage,
            "must not regenerate"
        );

        let trunc = good[..good.len() - 1].to_vec();
        std::fs::write(&file, &trunc).unwrap();
        expect_corrupt(&file);
        assert_eq!(std::fs::read(&file).unwrap(), trunc, "must not regenerate");

        let mut evil_pk = good.clone();
        evil_pk[40] ^= 0x01;
        std::fs::write(&file, &evil_pk).unwrap();
        expect_corrupt(&file);
        assert_eq!(
            std::fs::read(&file).unwrap(),
            evil_pk,
            "must not regenerate"
        );

        let mut evil_ver = good.clone();
        evil_ver[4] = 99;
        std::fs::write(&file, &evil_ver).unwrap();
        expect_corrupt(&file);
        assert_eq!(
            std::fs::read(&file).unwrap(),
            evil_ver,
            "must not regenerate"
        );

        std::fs::write(&file, &good).unwrap();
        let repaired = load_or_create(tmp.path()).unwrap();
        assert_eq!(repaired.device_id(), original.device_id());
    }

    #[test]
    fn missing_parent_directories_are_created() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a/b/c/identity");
        let _ = load_or_create(&deep).unwrap();
        assert!(deep.join(FILE_NAME).exists());
    }

    #[test]
    fn debug_output_never_contains_secret_material() {
        let secret = [0xA5u8; 32];
        let id = DeviceIdentity::from_secret_bytes(&secret);
        let rendered = format!("{id:?}");
        assert!(!rendered.contains("secret"));
        assert!(!rendered.to_lowercase().contains("a5a5a5"));

        assert_eq!(
            DeviceIdentity::from_file_bytes(Path::new("<m>"), &id.to_file_bytes())
                .unwrap()
                .device_id(),
            id.device_id()
        );
    }

    #[test]
    fn degenerate_peer_key_is_rejected() {
        let id = DeviceIdentity::generate();

        let zero = [0u8; 32];
        assert!(matches!(
            id.diffie_hellman(&zero),
            Err(IdentityError::DegeneratePeerKey)
        ));
    }
}
