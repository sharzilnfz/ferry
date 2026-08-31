


























use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_store::agreement::{AgreedRecord, AgreementLedger};
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::index::IndexEntry;
use ferry_store::manifest::{parse_manifest, parse_tree_node, EntryPayload};
use ferry_store::store::Store;

use crate::codec::{
    self, Bye, FolderOffer, IndexAdvert, ItemBatch, PackItem, RequestItems, RequestPacks,
};
use crate::error::{ByeReason, ProtoError};
use crate::secure::SecureSession;
use crate::stream::ByteStream;
use crate::version::ProtocolVersion;


const FOLDER_SENTINEL: [u8; 16] = [0; 16];



#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    
    Auto,
    
    ItemsOnly,
    
    PacksOnly,
}


pub struct FolderState {
    pub folder_id: [u8; 16],
    pub store: Arc<Store>,
    
    pub current_manifest: Option<BlobId>,
}



pub struct EngineConfig {
    pub identity: DeviceIdentity,
    
    pub expected_peer: DeviceId,
    pub folders: Vec<FolderState>,
    
    
    
    pub encryption: bool,
    pub granularity: Granularity,
    
    pub max_retries: u32,
}

impl EngineConfig {
    
    pub fn new(
        identity: DeviceIdentity,
        expected_peer: DeviceId,
        folders: Vec<FolderState>,
    ) -> Self {
        EngineConfig {
            identity,
            expected_peer,
            folders,
            encryption: true,
            granularity: Granularity::Auto,
            max_retries: 3,
        }
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderOutcome {
    pub folder_id: [u8; 16],
    
    
    pub local_manifest_after: Option<BlobId>,
    pub remote_manifest: Option<BlobId>,
    
    
    pub agreement_recorded: Option<BlobId>,
    
    
    pub rejections: usize,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionReport {
    pub peer: DeviceId,
    pub agreed_version: ProtocolVersion,
    pub encrypted: bool,
    pub folders: Vec<FolderOutcome>,
}


pub fn run_engine<S: ByteStream>(
    io: S,
    role: Role,
    cfg: EngineConfig,
) -> Result<SessionReport, ProtoError> {
    let our_max = ProtocolVersion::V1_0;
    let mut sess = SecureSession::establish(
        io,
        role,
        &cfg.identity,
        cfg.expected_peer,
        our_max,
        cfg.encryption,
    )?;

    let mut outcomes: Vec<FolderOutcome> = cfg
        .folders
        .iter()
        .map(|f| FolderOutcome {
            folder_id: f.folder_id,
            local_manifest_after: f.current_manifest,
            remote_manifest: None,
            agreement_recorded: None,
            rejections: 0,
        })
        .collect();

    if let Err(e) = folder_phases(&mut sess, role, &cfg, &mut outcomes) {
        return abort(&mut sess, e);
    }

    
    let bye_result = match role {
        Role::Initiator => sess
            .send_frame(
                codec::MSG_BYE,
                Bye {
                    reason: ByeReason::Normal,
                }
                .encode(),
            )
            .and_then(|()| sess.recv_expect_bye()),
        Role::Responder => sess.recv_expect_bye().and_then(|()| {
            sess.send_frame(
                codec::MSG_BYE,
                Bye {
                    reason: ByeReason::Normal,
                }
                .encode(),
            )
        }),
    };
    if let Err(e) = bye_result {
        return abort(&mut sess, e);
    }

    Ok(SessionReport {
        peer: sess.peer_id(),
        agreed_version: sess.version(),
        encrypted: sess.is_encrypted(),
        folders: outcomes,
    })
}



fn abort<S: ByteStream>(
    sess: &mut SecureSession<S>,
    err: ProtoError,
) -> Result<SessionReport, ProtoError> {
    if !matches!(err, ProtoError::ByeReceived { .. } | ProtoError::Io(_)) {
        let reason = match err {
            ProtoError::VersionIncompatible { .. } => ByeReason::VersionIncompatible,
            ProtoError::ProtocolViolation(_) | ProtoError::UnknownMessage { .. } => {
                ByeReason::ProtocolViolation
            }
            ProtoError::Auth(_) | ProtoError::IdentityMismatch { .. } => ByeReason::AuthFailed,
            ProtoError::FrameTooLarge { .. } | ProtoError::CounterExhausted => {
                ByeReason::ResourceLimit
            }
            ProtoError::ResourceLimit { .. } => ByeReason::ResourceLimit,
            _ => ByeReason::Internal,
        };
        let _ = sess.send_frame_best_effort(codec::MSG_BYE, Bye { reason }.encode());
    }
    Err(err)
}

fn store_err(e: ferry_store::store::StoreError) -> ProtoError {
    ProtoError::Io(std::io::Error::other(e.to_string()))
}

fn unexpected(t: u8) -> ProtoError {
    let _ = t;
    ProtoError::ProtocolViolation("unexpected message in this state")
}




#[derive(Clone, Copy, Debug)]
struct PeerFolder {
    manifest: Option<BlobId>,
}

type AdvertMap = BTreeMap<BlobId, IndexEntry>;


const BATCH_FLUSH_BYTES: usize = 8 * 1024 * 1024;

// Single budget capping BFS rounds, advert rows, and batches per round; the
// three former limits converged to the same value, so one constant avoids drift.
const BUDGET: usize = 262_144;
const MAX_BFS_ROUNDS: usize = BUDGET;












pub(crate) const MAX_ADVERT_ROWS_TOTAL: usize = BUDGET;





const MAX_BATCHES_PER_ROUND: usize = BUDGET;




const MAX_PACK_FRAMES_PER_ROUND: usize = 1_024;

fn now_secs_nsecs() -> (i64, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() as i64, now.subsec_nanos())
}

fn folder_phases<S: ByteStream>(
    sess: &mut SecureSession<S>,
    role: Role,
    cfg: &EngineConfig,
    outcomes: &mut [FolderOutcome],
) -> Result<(), ProtoError> {
    
    let (peer_folders, peer_adverts) = exchange_offers(sess, role, cfg, outcomes, true)?;
    for out in outcomes.iter_mut() {
        out.remote_manifest = peer_folders.get(&out.folder_id).and_then(|p| p.manifest);
    }

    
    
    
    let initiator_stage = stage_needed(cfg, &peer_folders, Role::Initiator, role);
    let responder_stage = stage_needed(cfg, &peer_folders, Role::Responder, role);

    match role {
        Role::Initiator => {
            if !initiator_stage.is_empty() {
                run_stage(
                    sess,
                    cfg,
                    &initiator_stage,
                    &peer_folders,
                    &peer_adverts,
                    outcomes,
                )?;
            }
            if !responder_stage.is_empty() {
                serve_stage(sess, cfg)?;
            }
        }
        Role::Responder => {
            if !initiator_stage.is_empty() {
                serve_stage(sess, cfg)?;
            }
            if !responder_stage.is_empty() {
                run_stage(
                    sess,
                    cfg,
                    &responder_stage,
                    &peer_folders,
                    &peer_adverts,
                    outcomes,
                )?;
            }
        }
    }

    
    finish_after_sync(sess, role, cfg, outcomes)
}







fn stage_needed(
    cfg: &EngineConfig,
    peer_folders: &BTreeMap<[u8; 16], PeerFolder>,
    whose: Role,
    my_role: Role,
) -> Vec<usize> {
    let mut out = Vec::new();
    for (idx, f) in cfg.folders.iter().enumerate() {
        let Some(pf) = peer_folders.get(&f.folder_id) else {
            continue; 
        };
        let (that_side, counterpart) = if whose == my_role {
            (f.current_manifest, pf.manifest)
        } else {
            (pf.manifest, f.current_manifest)
        };
        if pull_needed(that_side, counterpart) {
            out.push(idx);
        }
    }
    out
}

fn pull_needed(mine: Option<BlobId>, theirs: Option<BlobId>) -> bool {
    matches!(theirs, Some(t) if mine != Some(t))
}



fn run_stage<S: ByteStream>(
    sess: &mut SecureSession<S>,
    cfg: &EngineConfig,
    idxs: &[usize],
    peer_folders: &BTreeMap<[u8; 16], PeerFolder>,
    peer_adverts: &BTreeMap<[u8; 16], AdvertMap>,
    outcomes: &mut [FolderOutcome],
) -> Result<(), ProtoError> {
    for &idx in idxs {
        let f = &cfg.folders[idx];
        let Some(pf) = peer_folders.get(&f.folder_id) else {
            continue;
        };
        let Some(target) = pf.manifest else { continue };
        let empty = AdvertMap::new();
        let adverts = peer_adverts.get(&f.folder_id).unwrap_or(&empty);
        pull_folder(
            sess,
            f.folder_id,
            &f.store,
            target,
            f.current_manifest,
            adverts,
            cfg.granularity,
            cfg.max_retries,
            &mut outcomes[idx].rejections,
        )?;
        if f.current_manifest.is_none() {
            outcomes[idx].local_manifest_after = Some(target); 
        }
    }
    sess.send_frame(
        codec::MSG_REQUEST_ITEMS,
        RequestItems {
            folder_id: [0; 16],
            items: vec![],
        }
        .encode()?,
    )
}


fn serve_stage<S: ByteStream>(
    sess: &mut SecureSession<S>,
    cfg: &EngineConfig,
) -> Result<(), ProtoError> {
    loop {
        let Some(fb) = sess.recv_frame()? else {
            continue;
        };
        match fb.msg_type {
            codec::MSG_REQUEST_ITEMS => {
                let r = RequestItems::parse(&fb.payload)?;
                if r.items.is_empty() {
                    return Ok(());
                }
                serve_items(sess, cfg, r)?;
            }
            codec::MSG_REQUEST_PACKS => serve_packs(sess, cfg, RequestPacks::parse(&fb.payload)?)?,
            other => return Err(unexpected(other)),
        }
    }
}

fn find_store(cfg: &EngineConfig, folder_id: [u8; 16]) -> Result<&Arc<Store>, ProtoError> {
    cfg.folders
        .iter()
        .find(|f| f.folder_id == folder_id)
        .map(|f| &f.store)
        .ok_or_else(|| ProtoError::FolderUnknown {
            folder: hex(&folder_id),
        })
}

fn serve_items<S: ByteStream>(
    sess: &mut SecureSession<S>,
    cfg: &EngineConfig,
    r: RequestItems,
) -> Result<(), ProtoError> {
    let store = find_store(cfg, r.folder_id)?;
    let mut acc: Vec<(BlobKind, BlobId, Vec<u8>)> = Vec::new();
    let mut size = 0usize;
    for (kind, id) in r.items {
        if let Ok(bytes) = store.get(kind, &id) {
            size += bytes.len();
            acc.push((kind, id, bytes));
        }
        
        if acc.len() >= codec::MAX_BATCH_ITEMS || size >= BATCH_FLUSH_BYTES {
            let batch = std::mem::take(&mut acc);
            size = 0;
            sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch { items: batch }.encode()?)?;
        }
    }
    
    
    if !acc.is_empty() {
        sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch { items: acc }.encode()?)?;
    }
    sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch::TERMINATOR.encode()?)
}

fn serve_packs<S: ByteStream>(
    sess: &mut SecureSession<S>,
    cfg: &EngineConfig,
    r: RequestPacks,
) -> Result<(), ProtoError> {
    let store = find_store(cfg, r.folder_id)?;
    let packs_dir = store.store_dir().join("packs");
    for name in r.packs {
        let path = packs_dir.join(format!("{}.pack", hex(&name)));
        if let Ok(bytes) = std::fs::read(&path) {
            
            if *blake3::hash(&bytes).as_bytes() == name {
                sess.send_frame(
                    codec::MSG_PACK_ITEM,
                    PackItem { pack: name, bytes }.encode()?,
                )?;
            }
        }
    }
    sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch::TERMINATOR.encode()?)
}







#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn exchange_offers<S: ByteStream>(
    sess: &mut SecureSession<S>,
    role: Role,
    cfg: &EngineConfig,
    outcomes: &[FolderOutcome],
    with_adverts: bool,
) -> Result<
    (
        BTreeMap<[u8; 16], PeerFolder>,
        BTreeMap<[u8; 16], AdvertMap>,
    ),
    ProtoError,
> {
    let mut peer_folders: BTreeMap<[u8; 16], PeerFolder> = BTreeMap::new();
    let mut peer_adverts: BTreeMap<[u8; 16], AdvertMap> = BTreeMap::new();

    
    
    fn effective_manifest(
        cfg: &EngineConfig,
        outcomes: &[FolderOutcome],
        folder_id: [u8; 16],
    ) -> Option<BlobId> {
        outcomes
            .iter()
            .find(|o| o.folder_id == folder_id)
            .and_then(|o| o.local_manifest_after)
            .or_else(|| {
                cfg.folders
                    .iter()
                    .find(|f| f.folder_id == folder_id)
                    .and_then(|f| f.current_manifest)
            })
    }

    
    fn announce<T: ByteStream>(
        sess: &mut SecureSession<T>,
        cfg: &EngineConfig,
        outcomes: &[FolderOutcome],
        f: &FolderState,
        with_adverts: bool,
    ) -> Result<(), ProtoError> {
        sess.send_frame(
            codec::MSG_FOLDER_OFFER,
            FolderOffer {
                folder_id: f.folder_id,
                manifest_id: effective_manifest(cfg, outcomes, f.folder_id).unwrap_or([0; 32]),
                reserved: 0,
            }
            .encode(),
        )?;
        if with_adverts {
            send_my_adverts(sess, Some(&f.store))?;
        }
        Ok(())
    }

    
    fn echo<T: ByteStream>(
        sess: &mut SecureSession<T>,
        cfg: &EngineConfig,
        outcomes: &[FolderOutcome],
        folder_id: [u8; 16],
        with_adverts: bool,
    ) -> Result<(), ProtoError> {
        sess.send_frame(
            codec::MSG_FOLDER_OFFER,
            FolderOffer {
                folder_id,
                manifest_id: effective_manifest(cfg, outcomes, folder_id).unwrap_or([0; 32]),
                reserved: 0,
            }
            .encode(),
        )?;
        if with_adverts {
            match cfg
                .folders
                .iter()
                .find(|f| f.folder_id == folder_id)
                .map(|f| &f.store)
            {
                Some(store) => send_my_adverts(sess, Some(store))?,
                None => send_my_adverts(sess, None)?,
            }
        }
        Ok(())
    }

    
    
    fn consume_announcement<T: ByteStream>(
        po: FolderOffer,
        sess: &mut SecureSession<T>,
        with_adverts: bool,
        peer_folders: &mut BTreeMap<[u8; 16], PeerFolder>,
        peer_adverts: &mut BTreeMap<[u8; 16], AdvertMap>,
    ) -> Result<(), ProtoError> {
        let map = if with_adverts {
            recv_advert_map(sess)?
        } else {
            AdvertMap::new()
        };
        if with_adverts {
            peer_adverts.insert(po.folder_id, map);
        }
        peer_folders.insert(
            po.folder_id,
            PeerFolder {
                manifest: nonzero_manifest(po.manifest_id),
            },
        );
        Ok(())
    }

    
    
    fn consume_echo<S: ByteStream>(
        sess: &mut SecureSession<S>,
        with_adverts: bool,
        peer_folders: &mut BTreeMap<[u8; 16], PeerFolder>,
        peer_adverts: &mut BTreeMap<[u8; 16], AdvertMap>,
    ) -> Result<(), ProtoError> {
        let po = expect_offer(sess)?;
        let map = if with_adverts {
            recv_advert_map(sess)?
        } else {
            AdvertMap::new()
        };
        if with_adverts {
            peer_adverts.insert(po.folder_id, map);
        }
        peer_folders.insert(
            po.folder_id,
            PeerFolder {
                manifest: nonzero_manifest(po.manifest_id),
            },
        );
        Ok(())
    }

    let send_sentinel = |sess: &mut SecureSession<S>| -> Result<(), ProtoError> {
        sess.send_frame(
            codec::MSG_FOLDER_OFFER,
            FolderOffer {
                folder_id: FOLDER_SENTINEL,
                manifest_id: [0; 32],
                reserved: 0,
            }
            .encode(),
        )
    };

    match role {
        Role::Initiator => {
            for f in &cfg.folders {
                announce(sess, cfg, outcomes, f, with_adverts)?;
                consume_echo(sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
            }
            send_sentinel(sess)?;
            loop {
                let po = expect_offer(sess)?;
                if po.folder_id == FOLDER_SENTINEL {
                    break;
                }
                let folder_id = po.folder_id;
                consume_announcement(po, sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
                echo(sess, cfg, outcomes, folder_id, with_adverts)?;
            }
        }
        Role::Responder => {
            loop {
                let po = expect_offer(sess)?;
                if po.folder_id == FOLDER_SENTINEL {
                    break;
                }
                let folder_id = po.folder_id;
                consume_announcement(po, sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
                echo(sess, cfg, outcomes, folder_id, with_adverts)?;
            }
            for f in &cfg.folders {
                if peer_folders.contains_key(&f.folder_id) {
                    continue;
                }
                announce(sess, cfg, outcomes, f, with_adverts)?;
                consume_echo(sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
            }
            send_sentinel(sess)?;
        }
    }

    Ok((peer_folders, peer_adverts))
}

fn expect_offer<S: ByteStream>(sess: &mut SecureSession<S>) -> Result<FolderOffer, ProtoError> {
    let fb = sess.expect_frame(codec::MSG_FOLDER_OFFER)?;
    FolderOffer::parse(&fb.payload)
}

fn nonzero_manifest(id: BlobId) -> Option<BlobId> {
    if id == [0; 32] {
        None
    } else {
        Some(id)
    }
}




pub(crate) fn recv_advert_map<S: ByteStream>(
    sess: &mut SecureSession<S>,
) -> Result<AdvertMap, ProtoError> {
    let mut map = AdvertMap::new();
    let mut rows = 0usize;
    loop {
        let fb = sess.expect_frame(codec::MSG_INDEX_ADVERT)?;
        let adv = IndexAdvert::parse(&fb.payload)?;
        
        
        rows += adv.entries.len();
        if rows > MAX_ADVERT_ROWS_TOTAL {
            return Err(ProtoError::ResourceLimit {
                what: "advert rows for one folder",
                limit: MAX_ADVERT_ROWS_TOTAL,
            });
        }
        for e in adv.entries {
            map.insert(e.id, e);
        }
        if !adv.more {
            return Ok(map);
        }
    }
}



fn send_my_adverts<S: ByteStream>(
    sess: &mut SecureSession<S>,
    store: Option<&Arc<Store>>,
) -> Result<(), ProtoError> {
    let entries = match store {
        Some(s) => s.index_entries().map_err(store_err)?,
        None => Vec::new(),
    };
    if entries.is_empty() {
        sess.send_frame(
            codec::MSG_INDEX_ADVERT,
            IndexAdvert {
                entries: vec![],
                more: false,
            }
            .encode(),
        )?;
        return Ok(());
    }
    let chunks: Vec<&[IndexEntry]> = entries.chunks(IndexAdvert::MAX_ROWS).collect();
    let last = chunks.len() - 1;
    for (i, c) in chunks.into_iter().enumerate() {
        sess.send_frame(
            codec::MSG_INDEX_ADVERT,
            IndexAdvert {
                entries: c.to_vec(),
                more: i != last,
            }
            .encode(),
        )?;
    }
    Ok(())
}







#[allow(clippy::too_many_arguments)]
fn fetch_blobs<S: ByteStream>(
    sess: &mut SecureSession<S>,
    folder_id: [u8; 16],
    kind: BlobKind,
    want: &[BlobId],
    store: &Arc<Store>,
    retries: u32,
    rejections: &mut usize,
) -> Result<(), ProtoError> {
    let mut outstanding: Vec<BlobId> = want.to_vec();
    for _ in 0..=retries {
        if outstanding.is_empty() {
            return Ok(());
        }
        let mut got: BTreeSet<BlobId> = BTreeSet::new();
        for group in outstanding.chunks(codec::MAX_REQUEST_ITEMS) {
            sess.send_frame(
                codec::MSG_REQUEST_ITEMS,
                RequestItems {
                    folder_id,
                    items: group.iter().map(|id| (kind, *id)).collect(),
                }
                .encode()?,
            )?;
            got.extend(read_item_batches(sess, store, rejections)?);
        }
        outstanding.retain(|id| !got.contains(id));
    }
    if outstanding.is_empty() {
        Ok(())
    } else {
        Err(ProtoError::MissingItems(outstanding.len()))
    }
}




fn read_item_batches<S: ByteStream>(
    sess: &mut SecureSession<S>,
    store: &Arc<Store>,
    rejections: &mut usize,
) -> Result<BTreeSet<BlobId>, ProtoError> {
    let mut got = BTreeSet::new();
    let mut batches = 0usize;
    loop {
        batches += 1;
        if batches > MAX_BATCHES_PER_ROUND {
            return Err(ProtoError::ResourceLimit {
                what: "item batches in one request round",
                limit: MAX_BATCHES_PER_ROUND,
            });
        }
        let fb = sess.expect_frame(codec::MSG_ITEM_BATCH)?;
        let batch = ItemBatch::parse(&fb.payload)?;
        if batch.items.is_empty() {
            return Ok(got);
        }
        for (kind, id, bytes) in batch.items {
            
            
            if *blake3::hash(&bytes).as_bytes() != id {
                *rejections += 1;
                continue;
            }
            store.put_blob(kind, &bytes).map_err(store_err)?;
            got.insert(id);
        }
    }
}




fn fetch_via_packs<S: ByteStream>(
    sess: &mut SecureSession<S>,
    folder_id: [u8; 16],
    wanted: &[BlobId],
    adverts: &AdvertMap,
    gran: Granularity,
    store: &Arc<Store>,
    rejections: &mut usize,
) -> Result<BTreeSet<BlobId>, ProtoError> {
    let mut satisfied = BTreeSet::new();
    if matches!(gran, Granularity::ItemsOnly) || wanted.is_empty() {
        return Ok(satisfied);
    }
    let mut by_pack: BTreeMap<BlobId, Vec<BlobId>> = BTreeMap::new();
    for id in wanted {
        if let Some(e) = adverts.get(id) {
            by_pack.entry(e.pack).or_default().push(*id);
        }
    }
    let min_count = match gran {
        Granularity::Auto => 2,
        Granularity::PacksOnly | Granularity::ItemsOnly => 1,
    };
    let packs: Vec<BlobId> = by_pack
        .into_iter()
        .filter(|(_, ids)| ids.len() >= min_count)
        .map(|(p, _)| p)
        .collect();

    for group in packs.chunks(codec::MAX_REQUEST_PACKS) {
        sess.send_frame(
            codec::MSG_REQUEST_PACKS,
            RequestPacks {
                folder_id,
                packs: group.to_vec(),
            }
            .encode()?,
        )?;
        let mut frames = 0usize;
        loop {
            frames += 1;
            if frames > MAX_PACK_FRAMES_PER_ROUND {
                return Err(ProtoError::ResourceLimit {
                    what: "frames in one pack request round",
                    limit: MAX_PACK_FRAMES_PER_ROUND,
                });
            }
            let fb = sess.expect_frame_any(&[codec::MSG_PACK_ITEM, codec::MSG_ITEM_BATCH])?;
            if fb.msg_type == codec::MSG_PACK_ITEM {
                let item = PackItem::parse(&fb.payload)?;
                
                
                
                if *blake3::hash(&item.bytes).as_bytes() != item.pack {
                    *rejections += 1;
                    continue;
                }
                ingest_pack(store, &item.bytes)?;
                for id in wanted {
                    if satisfied.contains(id) {
                        continue;
                    }
                    if adverts.get(id).is_some_and(|e| e.pack == item.pack)
                        && store.get(BlobKind::DataChunk, id).is_ok()
                    {
                        satisfied.insert(*id);
                    }
                }
            } else {
                let b = ItemBatch::parse(&fb.payload)?;
                if b.items.is_empty() {
                    break;
                }
                return Err(unexpected(codec::MSG_ITEM_BATCH));
            }
        }
    }
    Ok(satisfied)
}











pub(crate) fn ingest_pack(store: &Arc<Store>, bytes: &[u8]) -> Result<BlobId, ProtoError> {
    let name = *blake3::hash(bytes).as_bytes();
    store.adopt_pack(&name, bytes).map_err(store_err)?;
    Ok(name)
}


#[allow(clippy::too_many_arguments)]
fn pull_folder<S: ByteStream>(
    sess: &mut SecureSession<S>,
    folder_id: [u8; 16],
    store: &Arc<Store>,
    target: BlobId,
    current: Option<BlobId>,
    adverts: &AdvertMap,
    gran: Granularity,
    retries: u32,
    rejections: &mut usize,
) -> Result<(), ProtoError> {
    
    fetch_blobs(
        sess,
        folder_id,
        BlobKind::Manifest,
        &[target],
        store,
        retries,
        rejections,
    )?;
    let man_bytes = store.get(BlobKind::Manifest, &target).map_err(store_err)?;
    let manifest = parse_manifest(&man_bytes)
        .map_err(|_| ProtoError::ProtocolViolation("peer manifest failed to parse"))?;

    
    
    let mut queue = vec![manifest.root_tree_id];
    let mut enqueued: BTreeSet<BlobId> = queue.iter().copied().collect();
    let mut wanted_chunks: BTreeSet<BlobId> = BTreeSet::new();
    let mut rounds = 0usize;
    while !queue.is_empty() {
        rounds += 1;
        if rounds > MAX_BFS_ROUNDS {
            return Err(ProtoError::MissingItems(queue.len()));
        }
        let batch = std::mem::take(&mut queue);
        fetch_blobs(
            sess,
            folder_id,
            BlobKind::TreeNode,
            &batch,
            store,
            retries,
            rejections,
        )?;
        for id in batch {
            let bytes = store.get(BlobKind::TreeNode, &id).map_err(store_err)?;
            let node = parse_tree_node(&bytes)
                .map_err(|_| ProtoError::ProtocolViolation("peer tree node failed to parse"))?;
            for e in node.entries {
                match e.payload {
                    EntryPayload::Dir { child_tree_id } => {
                        if enqueued.insert(child_tree_id) {
                            queue.push(child_tree_id);
                        }
                    }
                    EntryPayload::File { chunks, .. } => {
                        for (cid, _) in chunks {
                            wanted_chunks.insert(cid);
                        }
                    }
                    EntryPayload::Symlink { .. } => {}
                }
            }
        }
    }

    
    let wanted: Vec<BlobId> = wanted_chunks
        .into_iter()
        .filter(|id| store.get(BlobKind::DataChunk, id).is_err())
        .collect();

    
    let satisfied = fetch_via_packs(sess, folder_id, &wanted, adverts, gran, store, rejections)?;
    let leftover: Vec<BlobId> = wanted
        .into_iter()
        .filter(|id| !satisfied.contains(id))
        .collect();
    if !leftover.is_empty() {
        fetch_blobs(
            sess,
            folder_id,
            BlobKind::DataChunk,
            &leftover,
            store,
            retries,
            rejections,
        )?;
    }

    let _ = current; 
    Ok(())
}






fn finish_after_sync<S: ByteStream>(
    sess: &mut SecureSession<S>,
    role: Role,
    cfg: &EngineConfig,
    outcomes: &mut [FolderOutcome],
) -> Result<(), ProtoError> {
    let (peer_folders, _) = exchange_offers(sess, role, cfg, outcomes, false)?;

    for out in outcomes.iter_mut() {
        if let Some(pf) = peer_folders.get(&out.folder_id) {
            out.remote_manifest = pf.manifest;
        }
        let (mine_now, theirs_now) = (out.local_manifest_after, out.remote_manifest);
        if let (Some(mine), Some(theirs)) = (mine_now, theirs_now) {
            if mine == theirs {
                let store = find_store(cfg, out.folder_id)?;
                let ledger = AgreementLedger::new(store.store_dir());
                let (sec, nsec) = now_secs_nsecs();
                ledger
                    .record(
                        &out.folder_id,
                        &AgreedRecord {
                            peer_device_id: sess.peer_id(),
                            manifest_id: mine,
                            agreed_sec: sec,
                            agreed_nsec: nsec,
                        },
                    )
                    .map_err(|e| ProtoError::Io(std::io::Error::other(e.to_string())))?;
                out.agreement_recorded = Some(mine);
            }
        }
    }
    Ok(())
}
