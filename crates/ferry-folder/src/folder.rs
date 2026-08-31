use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferry_crypto::config_head::{parse_config_head, write_config_head, WrappedKeyEntry};
use ferry_crypto::folder_key::{generate_fmk, unwrap_folder_key, wrap_folder_key, Fmk};
use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_crypto::pack_cipher::ChaChaCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::store::{Store, StoreError};
use serde::{Deserialize, Serialize};

use crate::error::{CodeInto, FolderError, FolderResult};

pub const CONFIG_FILE: &str = "config";

pub const SETTINGS_FILE: &str = "settings.json";

pub const DOT_DIR: &str = ".ferry";

pub const SETTINGS_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub format_version: u32,

    pub folder_id: String,
    #[serde(default)]
    pub honor_gitignore: bool,
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub overrides: Vec<String>,
}

impl Settings {
    pub fn ignore_config(&self) -> ferry_ignore::IgnoreConfig {
        ferry_ignore::IgnoreConfig {
            honor_gitignore: self.honor_gitignore,
            presets: self.presets.clone(),
            overrides: self.overrides.clone(),
        }
    }
}

pub fn dot_dir(folder: &Path) -> PathBuf {
    folder.join(DOT_DIR)
}

pub fn is_initialized(root: &Path) -> bool {
    let dot = root.join(DOT_DIR);
    dot.is_dir() && dot.join(CONFIG_FILE).is_file()
}

pub fn state_dir(folder: &Path) -> PathBuf {
    dot_dir(folder)
}

pub fn load_rules(folder: &Path, settings: &Settings) -> FolderResult<ferry_ignore::FerryIgnore> {
    ferry_ignore::FerryIgnore::new(folder, &settings.ignore_config()).code(
        "ignore-rules",
        "fix or remove the offending line in ferry.ignore / .ferry/settings.json",
    )
}

pub fn load_ignore_policy(folder: &Path) -> FolderResult<ferry_ignore::FerryIgnore> {
    let settings_path = dot_dir(folder).join(SETTINGS_FILE);
    let cfg = if settings_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&settings_path) {
            if let Ok(settings) = serde_json::from_str::<Settings>(&content) {
                settings.ignore_config()
            } else {
                ferry_ignore::IgnoreConfig::default()
            }
        } else {
            ferry_ignore::IgnoreConfig::default()
        }
    } else {
        ferry_ignore::IgnoreConfig::default()
    };
    ferry_ignore::FerryIgnore::new(folder, &cfg).code(
        "ignore-rules",
        "fix or remove the offending line in ferry.ignore / .ferry/settings.json",
    )
}

pub fn create_folder(
    root: &Path,
    identity: &DeviceIdentity,
    folder_id: [u8; 16],
    poly: u64,
) -> FolderResult<(Store, Fmk)> {
    let fmk = generate_fmk();

    std::fs::create_dir_all(root).code("io", "check the path and permissions")?;
    let store = Store::create(root, fmk, Box::new(ChaChaCipher))
        .code("store", "is this path writable? does .ferry already exist?")
        .map_err(bad_store_hint)?;
    store
        .put_polynomial(poly)
        .code("store", "retry; if it persists the disk may be full")?;

    let wrapped = wrap_folder_key(&fmk, &folder_id, identity.public()).code(
        "crypto",
        "identity keys are local; retry with a fresh identity if this repeats",
    )?;
    let head = write_config_head(
        &folder_id,
        &[WrappedKeyEntry::new(*identity.public(), wrapped)],
    );
    write_config_file(root, &head)?;
    Ok((store, fmk))
}

pub fn adopt_folder(
    root: &Path,
    identity: &DeviceIdentity,
    folder_id: [u8; 16],
    fmk: &Fmk,
    poly: u64,
) -> FolderResult<Store> {
    std::fs::create_dir_all(root).code("io", "check the path and permissions")?;
    let store = Store::create(root, *fmk, Box::new(ChaChaCipher))
        .code("store", "is this path writable? does .ferry already exist?")
        .map_err(bad_store_hint)?;
    store
        .put_polynomial(poly)
        .code("store", "retry; if it persists the disk may be full")?;
    let wrapped = wrap_folder_key(fmk, &folder_id, identity.public()).code(
        "crypto",
        "identity keys are local; retry with a fresh identity if this repeats",
    )?;
    let head = write_config_head(
        &folder_id,
        &[WrappedKeyEntry::new(*identity.public(), wrapped)],
    );
    write_config_file(root, &head)?;
    Ok(store)
}

fn bad_store_hint(e: FolderError) -> FolderError {
    if e.message.contains("exists") {
        return FolderError::new(
            "already-initialized",
            "this directory already contains a .ferry store",
            "run `ferry status` to inspect it, or pick another directory",
        );
    }
    e
}

fn write_config_file(root: &Path, bytes: &[u8]) -> FolderResult<()> {
    let path = root.join(DOT_DIR).join(CONFIG_FILE);
    std::fs::write(&path, bytes).code("io", format!("could not write {}", path.display()))
}

pub struct OpenFolder {
    pub root: PathBuf,
    pub settings: Settings,
    pub folder_id: [u8; 16],
    pub poly: u64,
    pub store: Arc<Store>,
    pub rules: Arc<ferry_ignore::FerryIgnore>,
}

impl OpenFolder {
    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn state_dir(&self) -> PathBuf {
        state_dir(&self.root)
    }

    pub fn rules(&self) -> Arc<ferry_ignore::FerryIgnore> {
        Arc::clone(&self.rules)
    }

    pub fn ignore_policy(&self) -> Arc<dyn ferry_scan::IgnorePolicy> {
        Arc::clone(&self.rules) as Arc<dyn ferry_scan::IgnorePolicy>
    }
}

pub fn open_folder(root: &Path, identity: &DeviceIdentity) -> FolderResult<OpenFolder> {
    let dot = dot_dir(root);
    if !dot.is_dir() {
        return Err(FolderError::new(
            "not-a-folder",
            format!(
                "{} is not a Ferry folder (no {} directory)",
                root.display(),
                DOT_DIR
            ),
            "run `ferry init` there first, or pass the folder path explicitly",
        ));
    }

    let settings_path = dot.join(SETTINGS_FILE);
    let settings: Settings = serde_json::from_str(
        &std::fs::read_to_string(&settings_path)
            .code("io", format!("could not read {}", settings_path.display()))?,
    )
    .code(
        "settings-corrupt",
        format!(
            "restore or delete {} and re-run setup",
            settings_path.display()
        ),
    )?;
    if settings.format_version != SETTINGS_FORMAT_VERSION {
        return Err(FolderError::new(
            "settings-version",
            format!(
                "{} has format_version {}, this build understands {}",
                settings_path.display(),
                settings.format_version,
                SETTINGS_FORMAT_VERSION
            ),
            "upgrade Ferry, or re-init the folder",
        ));
    }
    let _folder_id = ferry_store::format::unhex::<16>(&settings.folder_id).ok_or_else(|| {
        FolderError::new(
            "settings-corrupt",
            format!(
                "folder_id in {} is not 32 hex chars",
                settings_path.display()
            ),
            "re-init the folder",
        )
    })?;

    let head_bytes = std::fs::read(dot.join(CONFIG_FILE)).code(
        "not-a-folder",
        "run `ferry init` there first, or pass the folder path explicitly",
    )?;
    let head = parse_config_head(&head_bytes).code(
        "config-corrupt",
        "the folder's key envelope is damaged; restore from backup or re-pair the folder",
    )?;
    let entry = head
        .entries
        .iter()
        .find(|e| e.device_pub == *identity.public())
        .ok_or_else(|| {
            FolderError::new(
                "not-shared-with-device",
                format!(
                    "folder {} was never shared with this device ({})",
                    hex(&head.folder_id),
                    short_device(identity.public())
                ),
                "ask the owning device to run `ferry pair` / `ferry share` again",
            )
        })?;
    let fmk = unwrap_folder_key(&entry.wrapped, &head.folder_id, identity).code(
        "key-unwrap",
        "your device.key may have changed; restore it or re-pair the folder",
    )?;

    let store = Arc::new(open_store(root, *fmk)?);
    let poly = find_polynomial(&store)?;
    let rules = Arc::new(load_rules(root, &settings)?);

    Ok(OpenFolder {
        root: root.to_path_buf(),
        settings,
        folder_id: head.folder_id,
        poly,
        store,
        rules,
    })
}

#[cfg(any(test, feature = "test-util"))]
pub fn open_or_create_test_store(
    root: &Path,
    identity: &DeviceIdentity,
) -> FolderResult<Arc<Store>> {
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    if dot_dir(root).is_dir() {
        return Ok(open_folder(root, identity)?.store);
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&root, &mut hasher);
    let seed = std::hash::Hasher::finish(&hasher);
    let mut folder_id = [0u8; 16];
    folder_id[..8].copy_from_slice(&seed.to_le_bytes());
    folder_id[8..].copy_from_slice(&seed.to_be_bytes());
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(seed));

    let (store, _fmk) = create_folder(root, identity, folder_id, poly)?;
    save_settings(
        root,
        &Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: hex(&folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )?;

    store.flush().code("store", "is this path writable?")?;
    store
        .write_index_snapshot()
        .code("store", "is this path writable?")?;
    Ok(Arc::new(store))
}

fn open_store(root: &Path, fmk: Fmk) -> FolderResult<Store> {
    Store::open(root, fmk, Box::new(ChaChaCipher)).map_err(|e| match e {
        StoreError::Io(io) => FolderError::new(
            "not-a-folder",
            format!("cannot open store under {}: {io}", root.display()),
            "run `ferry init` there first, or pass the folder path explicitly",
        ),
        other => FolderError::new(
            "store-open",
            other.to_string(),
            "if packs were damaged, `ferry-store` rebuild can recover indexes; see docs/store-format.md",
        ),
    })
}

pub fn find_polynomial(store: &Store) -> FolderResult<u64> {
    let entries = store.index_entries().code("store", "index unreadable")?;
    let ids: Vec<&BlobId> = entries
        .iter()
        .filter(|e| e.kind == BlobKind::Polynomial)
        .map(|e| &e.id)
        .collect();
    match ids.len() {
        1 => store.get_polynomial(ids[0]).code(
            "poly-missing",
            "the polynomial record is unreadable; the folder store is damaged",
        ),
        0 => Err(FolderError::new(
            "poly-missing",
            "no polynomial record found in this folder's store",
            "the store is incomplete; restore from backup or re-init",
        )),
        n => Err(FolderError::new(
            "poly-missing",
            format!("{n} polynomial records found; expected exactly one"),
            "the store is corrupted; restore from backup",
        )),
    }
}

pub const DEFAULT_FERRY_IGNORE: &str = "\
# ferry.ignore — what Ferry syncs in this folder (gitignore syntax).
#
# Layering (lowest wins conflicts first, last matching line wins):
#   built-in defaults < this file < applied presets < user overrides
#
# Built-in defaults already exclude: .env, .env.*, node_modules/,
# .DS_Store, Thumbs.db, desktop.ini, *.swp, *~   (see `ferry ignore --list`)
#
# Examples:
#   !.env              opt .env back IN (share-time secret scan will warn)
#   dist/              keep build output out
#   *.tsbuildinfo
";

pub fn write_default_ignore_if_absent(root: &Path) -> FolderResult<bool> {
    let path = root.join("ferry.ignore");
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, DEFAULT_FERRY_IGNORE)
        .code("io", format!("could not write {}", path.display()))?;
    Ok(true)
}

pub fn save_settings(root: &Path, settings: &Settings) -> FolderResult<()> {
    let final_path = dot_dir(root).join(SETTINGS_FILE);
    let tmp = dot_dir(root).join(format!("{SETTINGS_FILE}.tmp"));
    let body = serde_json::to_string_pretty(settings).expect("settings serialize");
    std::fs::write(&tmp, body).code("io", format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, &final_path)
        .code("io", format!("could not finalize {}", final_path.display()))?;
    Ok(())
}

pub fn load_settings(root: &Path) -> FolderResult<Settings> {
    let settings_path = dot_dir(root).join(SETTINGS_FILE);
    let content = std::fs::read_to_string(&settings_path)
        .code("io", format!("could not read {}", settings_path.display()))?;
    serde_json::from_str(&content).code(
        "settings-corrupt",
        format!(
            "restore or delete {} and re-run setup",
            settings_path.display()
        ),
    )
}

pub fn short_device(dev: &[u8; 32]) -> String {
    hex(dev)[..8].to_string()
}

pub fn unwrap_own_fmk(opened: &OpenFolder, identity: &DeviceIdentity) -> FolderResult<Fmk> {
    let head_bytes = std::fs::read(dot_dir(&opened.root).join(CONFIG_FILE)).code(
        "config-corrupt",
        "the folder's key envelope is missing or unreadable",
    )?;
    let head = parse_config_head(&head_bytes).code(
        "config-corrupt",
        "restore from backup or re-pair the folder",
    )?;
    let entry = head
        .entries
        .iter()
        .find(|e| e.device_pub == *identity.public())
        .ok_or_else(|| {
            FolderError::new(
                "not-shared-with-device",
                "this folder was not shared with this device",
                "run `ferry init` here or ask the owner to share again",
            )
        })?;
    let fmk = *unwrap_folder_key(&entry.wrapped, &head.folder_id, identity).code(
        "key-unwrap",
        "your device.key may have changed; restore it or re-pair",
    )?;
    Ok(fmk)
}

pub(crate) fn append_wrap_entry_for(
    root: &Path,
    folder_id: [u8; 16],
    recipient: &DeviceId,
    wrapped: &[u8; ferry_crypto::folder_key::WRAPPED_LEN],
) -> FolderResult<()> {
    let path = dot_dir(root).join(CONFIG_FILE);
    let bytes = std::fs::read(&path).code("config-corrupt", "missing key envelope")?;
    let head = parse_config_head(&bytes).code("config-corrupt", "restore from backup")?;
    if head.entries.iter().any(|e| e.device_pub == *recipient) {
        return Ok(());
    }
    let mut entries: Vec<_> = head.entries.clone();
    entries.push(WrappedKeyEntry::new(*recipient, *wrapped));
    let updated = write_config_head(&folder_id, &entries);

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &updated).code("io", format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).code("io", format!("cannot finalize {}", path.display()))?;
    Ok(())
}
