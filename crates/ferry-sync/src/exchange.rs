






































use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ferry_crypto::identity::DeviceId;
use ferry_proto::codec::{
    self, Bye, FolderOffer, IndexAdvert, ItemBatch, PackItem, RequestItems, RequestPacks,
};
use ferry_proto::error::{ByeReason, ProtoError};
use ferry_store::format::{hex, BlobId, BlobKind, PackId};
use ferry_store::index::IndexEntry;
use ferry_store::manifest::{parse_manifest, parse_tree_node, EntryPayload, RootManifest};
use ferry_store::store::Store;

use crate::engine::{IngestError, SessionError};
use crate::session::{Established, SessionIo};


struct PullOutcome {
    held: usize,
    diverged: bool,
}


const FOLDER_SENTINEL: [u8; 16] = [0; 16];


const BATCH_FLUSH_BYTES: usize = 8 * 1024 * 1024;


const BUDGET: usize = 64;

type AdvertMap = BTreeMap<BlobId, IndexEntry>;



pub trait ExchangeHost {
    
    fn status(&self, line: &str);
    
    fn bump_rejected(&self);
    
    fn tree_root(&self) -> &Path;
    
    
    
    
    fn pin_state_dir(&self) -> Option<&Path> {
        None
    }
    
    fn adopt(&self, bytes: &[u8], manifest: &RootManifest) -> Result<(), SessionError>;
    
    
    
    
    fn note_tree_mutation(&self) {}
    
    fn agree(&self, peer: DeviceId, bytes: &[u8], manifest_id: BlobId) -> Result<(), SessionError>;
}



pub struct CurrentState {
    pub id: BlobId,
    pub bytes: Vec<u8>,
    pub manifest: RootManifest,
}



pub fn run_v1_session<'x, H: ExchangeHost>(
    est: &'x mut Established<'_>,
    host: &'x H,
    store: &'x Store,
    folder_id: [u8; 16],
    my: CurrentState,
    max_retries: u32,
    initiator: bool,
) -> Result<(), SessionError> {
    let mut ex = Exchange {
        est,
        host,
        store,
        folder_id,
        max_retries,
        initiator,
        cur: my,
        peer_offer: None,
        peer_adverts: AdvertMap::new(),
    };

    
    ex.offer_round(true)?;

    
    
    
    
    
    
    
    
    let my_stage = ex.pull_needed();
    let peer_stage = ex.peer_pulls_from_us();
    if initiator {
        if my_stage {
            ex.my_pull_stage()?;
        }
        if peer_stage {
            ex.serve_peer_stage()?;
        }
    } else {
        
        
        if peer_stage {
            ex.serve_peer_stage()?;
        }
        if my_stage {
            ex.my_pull_stage()?;
        }
    }

    
    let peer_final = ex.offer_round(false)?;

    
    if peer_final == ex.cur.id && peer_final != [0u8; 32] {
        let bytes = std::mem::take(&mut ex.cur.bytes);
        ex.host.agree(ex.est.peer, &bytes, ex.cur.id)?;
        ex.cur.bytes = bytes;
        ex.host.status(&format!(
            "SESSION complete: agreed on {}",
            hex_short(&peer_final)
        ));
    }

    
    let bye = Bye {
        reason: ByeReason::Normal,
    }
    .encode();
    if initiator {
        ex.est.io.send_frame(codec::MSG_BYE, bye)?;
        ex.est.io.recv_bye()?;
    } else {
        ex.est.io.recv_bye()?;
        ex.est.io.send_frame(codec::MSG_BYE, bye)?;
    }
    Ok(())
}

struct Exchange<'x, 'e, H: ExchangeHost> {
    est: &'x mut Established<'e>,
    host: &'x H,
    store: &'x Store,
    folder_id: [u8; 16],
    max_retries: u32,
    cur: CurrentState,
    
    initiator: bool,
    
    peer_offer: Option<FolderOffer>,
    
    peer_adverts: AdvertMap,
}

impl<H: ExchangeHost> Exchange<'_, '_, H> {
    fn status(&self, line: &str) {
        self.host.status(line);
    }

    fn pull_needed(&self) -> bool {
        match &self.peer_offer {
            Some(po) => po.manifest_id != [0u8; 32] && po.manifest_id != self.cur.id,
            None => false,
        }
    }

    
    
    
    fn peer_pulls_from_us(&self) -> bool {
        match &self.peer_offer {
            Some(po) => self.cur.id != [0u8; 32] && po.manifest_id != self.cur.id,
            None => false,
        }
    }

    

    
    
    
    fn offer_round(&mut self, with_adverts: bool) -> Result<BlobId, SessionError> {
        let my_offer = FolderOffer {
            folder_id: self.folder_id,
            manifest_id: self.cur.id,
            reserved: 0,
        };
        let sentinel = FolderOffer {
            folder_id: FOLDER_SENTINEL,
            manifest_id: [0; 32],
            reserved: 0,
        };

        if self.initiator {
            self.send_offer(&my_offer, with_adverts)?;
            
            let echoed = self.consume_echo(with_adverts)?;
            self.send_offer(&sentinel, false)?;
            loop {
                let po = self.expect_offer()?;
                if po.folder_id == FOLDER_SENTINEL {
                    break;
                }
                self.echo_announcement(po, with_adverts)?;
            }
            return Ok(echoed);
        }

        
        
        let mut covered_ours: Option<BlobId> = None;
        loop {
            let po = self.expect_offer()?;
            if po.folder_id == FOLDER_SENTINEL {
                break;
            }
            if po.folder_id == self.folder_id {
                
                
                
                covered_ours = Some(po.manifest_id);
            }
            self.echo_announcement(po, with_adverts)?;
        }
        let peer_final = match covered_ours {
            Some(id) => id,
            None => {
                
                
                
                self.send_offer(&my_offer, with_adverts)?;
                self.consume_echo(with_adverts)?
            }
        };
        self.send_offer(&sentinel, false)?;
        Ok(peer_final)
    }

    fn send_offer(&mut self, offer: &FolderOffer, with_adverts: bool) -> Result<(), SessionError> {
        self.est
            .io
            .send_frame(codec::MSG_FOLDER_OFFER, offer.encode())?;
        if with_adverts {
            self.send_my_adverts()?;
        }
        Ok(())
    }

    
    
    fn consume_echo(&mut self, with_adverts: bool) -> Result<BlobId, SessionError> {
        let po = self.expect_offer()?;
        if po.folder_id != self.folder_id {
            return Err(ProtoError::ProtocolViolation("echo named another folder").into());
        }
        if with_adverts {
            self.peer_adverts = self.recv_advert_sequence()?;
        }
        self.peer_offer = Some(po.clone());
        Ok(po.manifest_id)
    }

    
    
    
    
    
    fn echo_announcement(
        &mut self,
        po: FolderOffer,
        with_adverts: bool,
    ) -> Result<(), SessionError> {
        let known = po.folder_id == self.folder_id;
        if with_adverts {
            let map = self.recv_advert_sequence()?;
            if known {
                self.peer_adverts = map;
                self.peer_offer = Some(po.clone());
            }
        } else if known {
            self.peer_offer = Some(po.clone());
        }
        let echo = FolderOffer {
            folder_id: po.folder_id,
            manifest_id: if known { self.cur.id } else { [0; 32] },
            reserved: 0,
        };
        self.est
            .io
            .send_frame(codec::MSG_FOLDER_OFFER, echo.encode())?;
        if with_adverts {
            if known {
                self.send_my_adverts()?;
            } else {
                self.est.io.send_frame(
                    codec::MSG_INDEX_ADVERT,
                    IndexAdvert {
                        entries: vec![],
                        more: false,
                    }
                    .encode(),
                )?;
            }
        }
        Ok(())
    }

    fn expect_offer(&mut self) -> Result<FolderOffer, SessionError> {
        let fb = self.est.io.expect_frame(codec::MSG_FOLDER_OFFER)?;
        Ok(FolderOffer::parse(&fb.payload)?)
    }

    
    
    fn send_my_adverts(&mut self) -> Result<(), SessionError> {
        let entries = self.store.index_entries().map_err(wire_store_err)?;
        send_adverts_of(&mut self.est.io, entries)
    }

    
    fn recv_advert_sequence(&mut self) -> Result<AdvertMap, SessionError> {
        let mut map = AdvertMap::new();
        loop {
            let fb = self.est.io.expect_frame(codec::MSG_INDEX_ADVERT)?;
            let adv = IndexAdvert::parse(&fb.payload)?;
            for e in adv.entries {
                map.insert(e.id, e);
            }
            if !adv.more {
                return Ok(map);
            }
        }
    }

    

    
    
    fn my_pull_stage(&mut self) -> Result<(), SessionError> {
        let target = match self.peer_offer.as_ref() {
            Some(po) if po.manifest_id != [0u8; 32] => po.manifest_id,
            _ => return self.close_stage(),
        };

        
        self.fetch_blobs(BlobKind::Manifest, &[target])?;
        let man_bytes = self
            .store
            .get(BlobKind::Manifest, &target)
            .map_err(wire_store_err)?;
        let man = parse_manifest(&man_bytes)
            .map_err(|_| ProtoError::ProtocolViolation("peer manifest failed to parse"))?;

        let mine_empty = self.cur.manifest.root_tree_id == crate::empty_tree_id();
        let theirs_empty = man.root_tree_id == crate::empty_tree_id();

        if man.root_tree_id == self.cur.manifest.root_tree_id {
            
            
            
            if lineage_newer(&man, &self.cur.manifest) {
                self.status(&format!(
                    "SESSION settling equal roots on newer manifest {}",
                    hex_short(&target)
                ));
                self.adopt(target, man_bytes, man)?;
            }
        } else if theirs_empty && !mine_empty {
            
            self.status("SESSION skipping empty peer offer (bootstrap guard)");
        } else {
            let outcome = self.pull_content(&man, target)?;
            if outcome.held == 0 && !outcome.diverged {
                self.adopt(target, man_bytes, man)?;
            } else if outcome.held > 0 {
                
                
                
                
                
                self.status(&format!(
                    "pin: held {} path(s) from peer {} (release with `ferry pin release`)",
                    outcome.held,
                    hex_short(&self.est.peer)
                ));
            }
        }

        self.close_stage()
    }

    
    
    
    
    
    fn pull_content(
        &mut self,
        man: &RootManifest,
        _remote_manifest_id: BlobId,
    ) -> Result<PullOutcome, SessionError> {
        
        let mut queue = vec![man.root_tree_id];
        let mut enqueued: BTreeSet<BlobId> = queue.iter().copied().collect();
        let mut rounds = 0usize;
        while !queue.is_empty() {
            rounds += 1;
            if rounds > BUDGET {
                return Err(ProtoError::MissingItems(queue.len()).into());
            }
            let batch = std::mem::take(&mut queue);
            self.fetch_blobs(BlobKind::TreeNode, &batch)?;
            for id in batch {
                let bytes = self
                    .store
                    .get(BlobKind::TreeNode, &id)
                    .map_err(wire_store_err)?;
                let node = parse_tree_node(&bytes)
                    .map_err(|_| ProtoError::ProtocolViolation("peer tree node failed to parse"))?;
                for e in node.entries {
                    if let EntryPayload::Dir { child_tree_id } = e.payload {
                        if enqueued.insert(child_tree_id) {
                            queue.push(child_tree_id);
                        }
                    }
                }
            }
        }

        let base_rec = ferry_store::agreement::AgreementLedger::new(self.store.store_dir())
            .get(&self.folder_id, &self.est.peer)
            .map_err(|e| SessionError::Other(format!("agreement ledger: {e}")))?;
        let base_manifest = match base_rec {
            Some(rec) => match self.store.get(BlobKind::Manifest, &rec.manifest_id) {
                Ok(bytes) => parse_manifest(&bytes).ok(),
                Err(_) => None,
            },
            None => None,
        };

        
        
        
        
        
        let now = crate::engine::now_parts();
        let state_dir = self
            .host
            .pin_state_dir()
            .unwrap_or_else(|| self.store.store_dir());
        let result = {
            let mut wire = WireFetch {
                io: &mut self.est.io,
                host: self.host,
                store: self.store,
                folder_id: self.folder_id,
                max_retries: self.max_retries,
                adverts: &self.peer_adverts,
            };
            let res = ferry_sync_engine::ConvergenceEngine::new(self.store, self.host.tree_root())
                .state_dir(state_dir)
                .at(now)
                .fetch_with(&mut wire)
                .converge(&self.cur.manifest, man, base_manifest.as_ref())
                .map_err(|e| SessionError::Apply(format!("{e}")))?;
            res
        };

        let held_count = result.held.len();

        let mutated = result.apply.mutations() > 0
            || !result.quarantined.is_empty()
            || !result.conflicts.is_empty();
        if mutated {
            self.host.note_tree_mutation();
        }
        if result.apply.mutations() > 0 || !result.quarantined.is_empty() {
            self.status(&format!(
                "SESSION converged: {} mutation(s), {} quarantined, {} conflict(s), {} held",
                result.apply.mutations(),
                result.quarantined.len(),
                result.conflicts.len(),
                held_count
            ));
        }

        
        
        let diverged = !result.quarantined.is_empty()
            || !result.conflicts.is_empty()
            || !result.send.is_empty();

        Ok(PullOutcome {
            held: held_count,
            diverged,
        })
    }

    
    
    
    
    
    fn close_stage(&mut self) -> Result<(), SessionError> {
        self.est.io.send_frame(
            codec::MSG_REQUEST_ITEMS,
            RequestItems {
                folder_id: self.folder_id,
                items: vec![],
            }
            .encode()?,
        )?;
        Ok(())
    }

    
    fn serve_peer_stage(&mut self) -> Result<(), SessionError> {
        loop {
            let Some(fb) = self.est.io.recv_frame()? else {
                continue;
            };
            match fb.msg_type {
                codec::MSG_REQUEST_ITEMS => {
                    let r = RequestItems::parse(&fb.payload)?;
                    if r.items.is_empty() {
                        return Ok(());
                    }
                    self.serve_items(r)?;
                }
                codec::MSG_REQUEST_PACKS => self.serve_packs(RequestPacks::parse(&fb.payload)?)?,
                _ => {
                    return Err(
                        ProtoError::ProtocolViolation("unexpected message while serving").into(),
                    )
                }
            }
        }
    }

    
    
    fn serve_items(&mut self, r: RequestItems) -> Result<(), SessionError> {
        if r.folder_id != self.folder_id {
            return Err(ProtoError::ProtocolViolation("request for unknown folder").into());
        }
        let mut acc: Vec<(BlobKind, BlobId, Vec<u8>)> = Vec::new();
        let mut size = 0usize;
        for (kind, id) in r.items {
            if let Ok(bytes) = self.store.get(kind, &id) {
                size += bytes.len();
                acc.push((kind, id, bytes));
            }
            if acc.len() >= codec::MAX_BATCH_ITEMS || size >= BATCH_FLUSH_BYTES {
                let batch = std::mem::take(&mut acc);
                self.est
                    .io
                    .send_frame(codec::MSG_ITEM_BATCH, ItemBatch { items: batch }.encode()?)?;
                size = 0;
            }
        }
        if !acc.is_empty() {
            self.est
                .io
                .send_frame(codec::MSG_ITEM_BATCH, ItemBatch { items: acc }.encode()?)?;
        }
        self.est
            .io
            .send_frame(codec::MSG_ITEM_BATCH, ItemBatch::TERMINATOR.encode()?)?;
        Ok(())
    }

    
    
    fn serve_packs(&mut self, r: RequestPacks) -> Result<(), SessionError> {
        if r.folder_id != self.folder_id {
            return Err(ProtoError::ProtocolViolation("request for unknown folder").into());
        }
        let packs_dir = self.store.store_dir().join("packs");
        for name in r.packs {
            let path = packs_dir.join(format!("{}.pack", hex(&name)));
            if let Ok(bytes) = std::fs::read(&path) {
                if *blake3::hash(&bytes).as_bytes() == name {
                    self.est.io.send_frame(
                        codec::MSG_PACK_ITEM,
                        PackItem { pack: name, bytes }.encode()?,
                    )?;
                }
            }
        }
        self.est
            .io
            .send_frame(codec::MSG_ITEM_BATCH, ItemBatch::TERMINATOR.encode()?)?;
        Ok(())
    }

    

    
    
    
    fn fetch_blobs(&mut self, kind: BlobKind, want: &[BlobId]) -> Result<(), SessionError> {
        fetch_blobs(
            &mut self.est.io,
            self.host,
            self.store,
            self.folder_id,
            self.max_retries,
            kind,
            want,
        )
    }

    
    
    fn adopt(&mut self, id: BlobId, bytes: Vec<u8>, man: RootManifest) -> Result<(), SessionError> {
        
        
        self.store
            .put_meta(BlobKind::Manifest, &bytes)
            .map_err(wire_store_err)?;
        self.host.adopt(&bytes, &man)?;
        self.cur = CurrentState {
            id,
            bytes,
            manifest: man,
        };
        Ok(())
    }
}



fn hex_short(b: &BlobId) -> String {
    hex(b)[..12].to_string()
}

fn wire_store_err(e: ferry_store::store::StoreError) -> ProtoError {
    ProtoError::Io(std::io::Error::other(e.to_string()))
}






struct WireFetch<'x, 'e, H: ExchangeHost> {
    io: &'x mut SessionIo<'e>,
    host: &'x H,
    store: &'x Store,
    folder_id: [u8; 16],
    max_retries: u32,
    adverts: &'x AdvertMap,
}

impl<H: ExchangeHost> ferry_sync_engine::BlobFetch for WireFetch<'_, '_, H> {
    fn fetch(
        &mut self,
        want: &[(ferry_store::format::BlobId, u64)],
    ) -> Result<(), ferry_sync_engine::ConvergenceError> {
        let wanted: Vec<BlobId> = want.iter().map(|(id, _)| *id).collect();
        let satisfied = fetch_via_packs(
            self.io,
            self.host,
            self.store,
            self.folder_id,
            self.adverts,
            &wanted,
        )
        .map_err(session_to_convergence)?;
        let leftover: Vec<BlobId> = wanted
            .iter()
            .filter(|id| !satisfied.contains(*id))
            .copied()
            .collect();
        if !leftover.is_empty() {
            fetch_blobs(
                self.io,
                self.host,
                self.store,
                self.folder_id,
                self.max_retries,
                BlobKind::DataChunk,
                &leftover,
            )
            .map_err(session_to_convergence)?;
        }
        Ok(())
    }
}

fn session_to_convergence(e: SessionError) -> ferry_sync_engine::ConvergenceError {
    ferry_sync_engine::ConvergenceError::Fetch(e.to_string())
}




fn fetch_blobs<H: ExchangeHost>(
    io: &mut SessionIo<'_>,
    host: &H,
    store: &Store,
    folder_id: [u8; 16],
    max_retries: u32,
    kind: BlobKind,
    want: &[BlobId],
) -> Result<(), SessionError> {
    let mut outstanding: Vec<BlobId> = want.to_vec();
    for _round in 0..=max_retries {
        if outstanding.is_empty() {
            return Ok(());
        }
        let mut got: BTreeSet<BlobId> = BTreeSet::new();
        for group in outstanding.chunks(codec::MAX_REQUEST_ITEMS) {
            io.send_frame(
                codec::MSG_REQUEST_ITEMS,
                RequestItems {
                    folder_id,
                    items: group.iter().map(|id| (kind, *id)).collect(),
                }
                .encode()?,
            )?;
            got.extend(read_item_batches(io, host, store, kind)?);
        }
        outstanding.retain(|id| !got.contains(id));
    }
    if outstanding.is_empty() {
        Ok(())
    } else {
        Err(ProtoError::MissingItems(outstanding.len()).into())
    }
}




fn read_item_batches<H: ExchangeHost>(
    io: &mut SessionIo<'_>,
    host: &H,
    store: &Store,
    expected_kind: BlobKind,
) -> Result<BTreeSet<BlobId>, SessionError> {
    let mut got = BTreeSet::new();
    loop {
        let fb = io.expect_frame(codec::MSG_ITEM_BATCH)?;
        let batch = ItemBatch::parse(&fb.payload)?;
        if batch.items.is_empty() {
            return Ok(got);
        }
        for (kind, id, bytes) in batch.items {
            if kind != expected_kind {
                return Err(ProtoError::ProtocolViolation("wrong blob kind in batch").into());
            }
            if *blake3::hash(&bytes).as_bytes() != id {
                host.bump_rejected();
                continue;
            }
            store.put_blob(kind, &bytes).map_err(wire_store_err)?;
            got.insert(id);
        }
    }
}





fn fetch_via_packs<H: ExchangeHost>(
    io: &mut SessionIo<'_>,
    host: &H,
    store: &Store,
    folder_id: [u8; 16],
    adverts: &AdvertMap,
    wanted: &[BlobId],
) -> Result<BTreeSet<BlobId>, SessionError> {
    let mut satisfied = BTreeSet::new();
    if wanted.is_empty() {
        return Ok(satisfied);
    }
    let mut by_pack: BTreeMap<PackId, Vec<BlobId>> = BTreeMap::new();
    for id in wanted {
        if let Some(e) = adverts.get(id) {
            by_pack.entry(e.pack).or_default().push(*id);
        }
    }
    
    let packs: Vec<PackId> = by_pack
        .into_iter()
        .filter(|(_, ids)| ids.len() >= 2)
        .map(|(p, _)| p)
        .collect();

    let mut landed_pack = false;
    for group in packs.chunks(codec::MAX_REQUEST_PACKS) {
        io.send_frame(
            codec::MSG_REQUEST_PACKS,
            RequestPacks {
                folder_id,
                packs: group.to_vec(),
            }
            .encode()?,
        )?;
        loop {
            let fb = io.expect_frame_any(&[codec::MSG_PACK_ITEM, codec::MSG_ITEM_BATCH])?;
            if fb.msg_type == codec::MSG_PACK_ITEM {
                let item = PackItem::parse(&fb.payload)?;
                if *blake3::hash(&item.bytes).as_bytes() != item.pack {
                    host.bump_rejected();
                    continue;
                }
                ingest_pack_verified(store, &item.pack, &item.bytes)?;
                landed_pack = true;
                for &id in wanted {
                    if satisfied.contains(&id) {
                        continue;
                    }
                    if adverts.get(&id).is_some_and(|e| e.pack == item.pack)
                        && store.get(BlobKind::DataChunk, &id).is_ok()
                    {
                        satisfied.insert(id);
                    }
                }
            } else {
                let b = ItemBatch::parse(&fb.payload)?;
                if b.items.is_empty() {
                    break;
                }
                return Err(ProtoError::ProtocolViolation(
                    "unexpected nonempty batch during pack transfer",
                )
                .into());
            }
        }
    }
    if landed_pack {
        
        
        
        
        store.flush().map_err(wire_store_err)?;
    }
    Ok(satisfied)
}



pub(crate) fn lineage_newer(candidate: &RootManifest, incumbent: &RootManifest) -> bool {
    let ka = (
        candidate.created_sec,
        candidate.created_nsec,
        candidate.device_id,
        candidate.root_tree_id,
    );
    let kb = (
        incumbent.created_sec,
        incumbent.created_nsec,
        incumbent.device_id,
        incumbent.root_tree_id,
    );
    ka > kb
}





pub fn ingest_pack_verified(
    store: &Store,
    claimed_name: &PackId,
    bytes: &[u8],
) -> Result<(), IngestError> {
    match store.adopt_pack(claimed_name, bytes) {
        Ok(()) => Ok(()),
        Err(ferry_store::store::StoreError::Pack(ferry_store::pack::PackError::NameMismatch {
            expected,
            found,
        })) => Err(IngestError::NameMismatch {
            claimed: expected,
            found,
        }),
        Err(other) => Err(IngestError::Other(other.to_string())),
    }
}



fn send_adverts_of(io: &mut SessionIo, entries: Vec<IndexEntry>) -> Result<(), SessionError> {
    if entries.is_empty() {
        io.send_frame(
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
        io.send_frame(
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
