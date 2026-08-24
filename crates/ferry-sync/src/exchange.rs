//! The protocol v1 conversation driver: ferry-sync's half of
//! `docs/store-format.md` §"Wire protocol v1", riding [`crate::session`]'s
//! sealed frames. Message-for-message it mirrors the reference engine
//! (`ferry_proto::run_engine`) so the two interoperate byte for byte;
//! `tests/protocol_v1.rs` proves that over real TCP in both role
//! assignments.
//!
//! Where M0 elected a single donor/puller per session, v1's conversation
//! is symmetric and role-serialized: offers with adverts first (initiator
//! announces, responder mirrors), then at most one pull stage per side —
//! initiator pulls first, responder second — each ended by an empty
//! `REQUEST_ITEMS` marker answered by a bare empty `ITEM_BATCH`. A second
//! offer round without adverts makes post-pull equality observable;
//! equal, nonzero manifest ids record the last-agreed pointer LOCALLY on
//! each side (no wire message). BYE closes: initiator sends, responder
//! mirrors.
//!
//! Pull-stage decisions preserve the M0 semantics this skeleton is
//! accepted against:
//!
//! - **Bootstrap guard**: a peer whose offered root tree is EMPTY never
//!   pulls content over our non-empty state (the fresh-empty device loses
//!   the bootstrap race, exactly like `pick_donor`'s rule 1).
//! - **Last-writer-wins adoption**: after fetching the peer manifest we
//!   adopt it as our current state only when its lineage beats ours (or
//!   the roots are equal and theirs wins the tie). The loser of a race
//!   therefore adopts nothing and keeps its pointer, so round-2 ids
//!   converge instead of ping-ponging across polls.
//! - **Materialize before round 2**: the puller applies the change set to
//!   its working tree durably BEFORE the second offer round, mirroring
//!   M0's "materialize, THEN confirm" order; round 2 plays AGREED's role.
//!
//! Integrity rules are the normative ones: packs verify
//! `BLAKE3(ciphertext) == claimed name` BEFORE anything is written or
//! decrypted; every blob verifies `BLAKE3(plaintext) == claimed id`
//! BEFORE touching the store; missing or rejected items are re-requested
//! up to the retry budget, then the session fails cleanly. A corrupted
//! transfer is thus never applied, and the next poll round converges.

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

use crate::applier::SessionApplier;
use crate::engine::{IngestError, SessionError};
use crate::session::{Established, SessionIo};

/// Zero `folder_id` marks "end of announcement list" in offer rounds.
const FOLDER_SENTINEL: [u8; 16] = [0; 16];

/// Payload flush threshold for `ITEM_BATCH` frames (8 MiB, normative limit).
const BATCH_FLUSH_BYTES: usize = 8 * 1024 * 1024;

/// BFS round guard for remote tree walks.
const MAX_BFS_ROUNDS: usize = 64;

/// Peer index entries for one folder, keyed by blob id.
type AdvertMap = BTreeMap<BlobId, IndexEntry>;

/// Engine-facing callbacks so the driver stays decoupled from snapshot
/// pointers, stats, ledgers, and stdout.
pub trait ExchangeHost {
    /// Quiet-able status line.
    fn status(&self, line: &str);
    /// Count one refused transfer (tag, hash, or pack-name verification).
    fn bump_rejected(&self);
    /// The working tree materialization applies to.
    fn tree_root(&self) -> &Path;
    /// Adopt `manifest` as our current folder state (no agreement yet).
    fn adopt(&self, bytes: &[u8], manifest: &RootManifest) -> Result<(), SessionError>;
    /// Record the last-agreed pointer against `peer`, locally.
    fn agree(&self, peer: DeviceId, bytes: &[u8], manifest_id: BlobId) -> Result<(), SessionError>;
}

/// The manifest we currently announce as ours. Adoption replaces it
/// mid-session so round-2 announcements reflect reality.
pub struct CurrentState {
    pub id: BlobId,
    pub bytes: Vec<u8>,
    pub manifest: RootManifest,
}

/// Run one full v1 conversation on an established session: offers → pull
/// stages → round 2 → local agreement → BYE.
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

    // Round 1: announcements WITH adverts.
    ex.offer_round(true)?;

    // Pull stages are strictly serialized by ROLE; BOTH stage plans are
    // computable from round-1 state alone, so no extra messages are
    // needed. The conditions are deliberately asymmetric at the zero
    // edges, mirroring the reference engine: a side whose OWN pointer is
    // nothing yet always pulls whatever the other side holds, while a
    // side holding nothing for the folder is never pulled FROM. Stage
    // plans are FIXED here — an adoption during the first stage does not
    // cancel the second side's planned stage.
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
        // Responder: the initiator's stage comes first (we serve), ours
        // second.
        if peer_stage {
            ex.serve_peer_stage()?;
        }
        if my_stage {
            ex.my_pull_stage()?;
        }
    }

    // Round 2 WITHOUT adverts: observe post-pull equality.
    let peer_final = ex.offer_round(false)?;

    // Agreement is LOCAL: equal, nonzero ids on both sides record.
    if peer_final == ex.cur.id && peer_final != [0u8; 32] {
        let bytes = std::mem::take(&mut ex.cur.bytes);
        ex.host.agree(ex.est.peer, &bytes, ex.cur.id)?;
        ex.cur.bytes = bytes;
        ex.host.status(&format!(
            "SESSION complete: agreed on {}",
            hex_short(&peer_final)
        ));
    }

    // Bye phase: initiator sends first, responder mirrors.
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
    /// Our role in this conversation (dialer speaks first).
    initiator: bool,
    /// The peer's announced state for OUR shared folder.
    peer_offer: Option<FolderOffer>,
    /// The peer's round-1 advert rows for our folder (request grouping).
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

    /// Whether the PEER's stage exists: it wants to pull from us. Same
    /// rule evaluated from its seat — its pointer may be NOTHING (zero
    /// offer), in which case it always pulls whatever we hold.
    fn peer_pulls_from_us(&self) -> bool {
        match &self.peer_offer {
            Some(po) => self.cur.id != [0u8; 32] && po.manifest_id != self.cur.id,
            None => false,
        }
    }

    // --- offer / advert rounds -------------------------------------------------

    /// Announce + mirror one round. With `with_adverts` every offer
    /// carries the sender's advert sequence for the named folder. Returns
    /// the peer's final announced manifest id for our folder.
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
            // The echo IS how the initiator learns the peer's state.
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

        // Responder: mirror announcements until the initiator's sentinel,
        // then announce folders only we have, then end the list.
        let mut covered_ours: Option<BlobId> = None;
        loop {
            let po = self.expect_offer()?;
            if po.folder_id == FOLDER_SENTINEL {
                break;
            }
            if po.folder_id == self.folder_id {
                // The announcement IS the peer's current state for our
                // folder — in round 2 this is exactly the post-pull id we
                // must compare against.
                covered_ours = Some(po.manifest_id);
            }
            self.echo_announcement(po, with_adverts)?;
        }
        let peer_final = match covered_ours {
            Some(id) => id,
            None => {
                // The initiator never announced our folder ("folders only
                // the responder has"): announce it ourselves and consume
                // the echo.
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

    /// Read the peer's echo of OUR announcement plus its advert tail. The
    /// echo IS how the initiator learns the peer's state.
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

    /// Mirror an announcement back with OUR state (zeros + empty advert
    /// for folders we do not share). The announcement's OWN advert tail is
    /// consumed FIRST — every announcement carries at least one advert,
    /// and leaving it queued would desynchronize the round. An
    /// announcement naming OUR folder is also the peer's state for it.
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

    /// Send our index entries chunked at the normative row cap; at least
    /// one advert always goes out, even when empty.
    fn send_my_adverts(&mut self) -> Result<(), SessionError> {
        let entries = self.store.index_entries().map_err(wire_store_err)?;
        send_adverts_of(&mut self.est.io, entries)
    }

    /// Read one folder's advert sequence up to the closing `more=0`.
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

    // --- pull stages --------------------------------------------------------------

    /// MY stage: fetch the peer manifest, decide, maybe pull content,
    /// materialize durably, adopt, then close the stage with the marker.
    fn my_pull_stage(&mut self) -> Result<(), SessionError> {
        let target = match self.peer_offer.as_ref() {
            Some(po) if po.manifest_id != [0u8; 32] => po.manifest_id,
            _ => return self.close_stage(),
        };

        // 1. The peer's root manifest, by id, verified after receipt.
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
            // Same content, different lineage: settle like M0's same-root
            // path — the LINEAGE WINNER's manifest becomes both sides'
            // pointer, computed identically on each side.
            if lineage_newer(&man, &self.cur.manifest) {
                self.status(&format!(
                    "SESSION settling equal roots on newer manifest {}",
                    hex_short(&target)
                ));
                self.adopt(target, man_bytes, man)?;
            }
        } else if theirs_empty && !mine_empty {
            // Bootstrap guard: never trade content for emptiness.
            self.status("SESSION skipping empty peer offer (bootstrap guard)");
        } else if !mine_empty && !lineage_newer(&man, &self.cur.manifest) {
            // Stale offer: the peer announced a manifest OLDER than what
            // we already hold (it announced before it saw our latest, or
            // it still runs our own earlier state). Adopting would
            // REGRESS; skip and let the peer catch up from us instead —
            // this is M0's single-direction flow, recovered through
            // lineage instead of a donor message. A FRESH device (empty
            // tree) bypasses this guard: bootstrap adoption ignores the
            // clock, exactly like pick_donor's rule 1.
            self.status("SESSION skipping stale peer offer (lineage guard)");
        } else {
            self.pull_content(&man)?;
            self.adopt(target, man_bytes, man)?;
        }

        self.close_stage()
    }

    /// Tree walk + diff + packs-first data fetch + durable materialize.
    fn pull_content(&mut self, man: &RootManifest) -> Result<(), SessionError> {
        // 2. Breadth-first walk of the peer's tree: fetch missing nodes.
        let mut queue = vec![man.root_tree_id];
        let mut enqueued: BTreeSet<BlobId> = queue.iter().copied().collect();
        let mut rounds = 0usize;
        while !queue.is_empty() {
            rounds += 1;
            if rounds > MAX_BFS_ROUNDS {
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

        // 3. What we actually want: the diff's chunks, minus what we hold.
        //    Entries outside the diff buckets are identical in both
        //    manifests (same chunk lists), and anything in OUR manifest
        //    was ingested locally, so diff coverage is sound.
        let changes = ferry_store::diff::diff_manifests(self.store, &self.cur.manifest, man)?;
        let wanted: Vec<BlobId> = crate::engine::collect_chunk_ids_public(&changes)
            .into_iter()
            .filter(|id| self.store.get(BlobKind::DataChunk, id).is_err())
            .collect();
        self.status(&format!(
            "SESSION pulling: {} added / {} removed / {} modified / {} metadata, {} chunks wanted",
            changes.added.len(),
            changes.removed.len(),
            changes.content_modified.len() + changes.type_changed.len(),
            changes.metadata_modified.len(),
            wanted.len()
        ));

        // 4. Packs first where the ADVERTISED grouping pays off (Auto
        //    granularity: >= 2 wanted chunks share one pack).
        let satisfied = self.fetch_via_packs(&wanted)?;

        // 5. Remainder item-level.
        let leftover: Vec<BlobId> = wanted
            .iter()
            .filter(|id| !satisfied.contains(*id))
            .copied()
            .collect();
        if !leftover.is_empty() {
            self.fetch_blobs(BlobKind::DataChunk, &leftover)?;
        }

        // 6. Materialize durably into the working tree BEFORE round 2.
        SessionApplier::new(self.store, self.host.tree_root())
            .apply(man, &changes)
            .map_err(|e| SessionError::Apply(format!("{e}")))?;
        Ok(())
    }

    /// Close MY stage: the empty `REQUEST_ITEMS` marker. Per the reference
    /// conversation, the server answers NOTHING — it returns to listening
    /// (only item/pack responses carry `ITEM_BATCH` terminators). Matching
    /// those bytes is what keeps us interoperable with
    /// `ferry_proto::run_engine`.
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

    /// THEIR stage: answer requests until the end-of-stage marker arrives.
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

    /// Serve `REQUEST_ITEMS` from the store, batching at the normative caps;
    /// unserved ids are omitted (the requester detects gaps and retries).
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

    /// Serve `REQUEST_PACKS`: whole ciphertext files under their names, and
    /// only when the bytes hash to the name (never serve damaged packs).
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

    // --- verified fetches ------------------------------------------------------

    /// Fetch blobs of one kind by id: request in batches, verify EVERY
    /// received item AFTER decryption, store only verified bytes, detect
    /// gaps, retry within budget. Missing ids at exhaustion fail cleanly.
    fn fetch_blobs(&mut self, kind: BlobKind, want: &[BlobId]) -> Result<(), SessionError> {
        let mut outstanding: Vec<BlobId> = want.to_vec();
        for _round in 0..=self.max_retries {
            if outstanding.is_empty() {
                return Ok(());
            }
            let mut got: BTreeSet<BlobId> = BTreeSet::new();
            for group in outstanding.chunks(codec::MAX_REQUEST_ITEMS) {
                self.est.io.send_frame(
                    codec::MSG_REQUEST_ITEMS,
                    RequestItems {
                        folder_id: self.folder_id,
                        items: group.iter().map(|id| (kind, *id)).collect(),
                    }
                    .encode()?,
                )?;
                got.extend(self.read_item_batches(kind)?);
            }
            outstanding.retain(|id| !got.contains(id));
        }
        if outstanding.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::MissingItems(outstanding.len()).into())
        }
    }

    /// Read `ITEM_BATCH` frames until the terminator. Verify-after-decrypt:
    /// BLAKE3(plaintext) MUST equal the claimed id BEFORE anything touches
    /// the store; rejects are counted and re-requested by the retry loop.
    fn read_item_batches(
        &mut self,
        expected_kind: BlobKind,
    ) -> Result<BTreeSet<BlobId>, SessionError> {
        let mut got = BTreeSet::new();
        loop {
            let fb = self.est.io.expect_frame(codec::MSG_ITEM_BATCH)?;
            let batch = ItemBatch::parse(&fb.payload)?;
            if batch.items.is_empty() {
                return Ok(got);
            }
            for (kind, id, bytes) in batch.items {
                if kind != expected_kind {
                    return Err(ProtoError::ProtocolViolation("wrong blob kind in batch").into());
                }
                if *blake3::hash(&bytes).as_bytes() != id {
                    self.host.bump_rejected();
                    continue;
                }
                self.store.put_blob(kind, &bytes).map_err(wire_store_err)?;
                got.insert(id);
            }
        }
    }

    /// Pack-granular fetch grouped through the SERVER'S advertised index
    /// entries. Returns the wanted ids satisfied through packs. Pack
    /// integrity: BLAKE3(ciphertext) == claimed name BEFORE storing or
    /// decrypting anything.
    fn fetch_via_packs(&mut self, wanted: &[BlobId]) -> Result<BTreeSet<BlobId>, SessionError> {
        let mut satisfied = BTreeSet::new();
        if wanted.is_empty() {
            return Ok(satisfied);
        }
        let mut by_pack: BTreeMap<PackId, Vec<BlobId>> = BTreeMap::new();
        for id in wanted {
            if let Some(e) = self.peer_adverts.get(id) {
                by_pack.entry(e.pack).or_default().push(*id);
            }
        }
        // Auto granularity: whole pack only when >= 2 wanted chunks share it.
        let packs: Vec<PackId> = by_pack
            .into_iter()
            .filter(|(_, ids)| ids.len() >= 2)
            .map(|(p, _)| p)
            .collect();

        let mut landed_pack = false;
        for group in packs.chunks(codec::MAX_REQUEST_PACKS) {
            self.est.io.send_frame(
                codec::MSG_REQUEST_PACKS,
                RequestPacks {
                    folder_id: self.folder_id,
                    packs: group.to_vec(),
                }
                .encode()?,
            )?;
            loop {
                let fb = self
                    .est
                    .io
                    .expect_frame_any(&[codec::MSG_PACK_ITEM, codec::MSG_ITEM_BATCH])?;
                if fb.msg_type == codec::MSG_PACK_ITEM {
                    let item = PackItem::parse(&fb.payload)?;
                    if *blake3::hash(&item.bytes).as_bytes() != item.pack {
                        self.host.bump_rejected();
                        continue;
                    }
                    ingest_pack_verified(self.store, &item.pack, &item.bytes)?;
                    landed_pack = true;
                    for &id in wanted {
                        if satisfied.contains(&id) {
                            continue;
                        }
                        if self
                            .peer_adverts
                            .get(&id)
                            .is_some_and(|e| e.pack == item.pack)
                            && self.store.get(BlobKind::DataChunk, &id).is_ok()
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
            // Fold freshly landed packs into the location table once per
            // stage (M0 shortcut; T-002/T-008 replace with incremental).
            self.store.flush().map_err(wire_store_err)?;
            let (_, skipped) = self.store.rebuild_index().map_err(wire_store_err)?;
            if !skipped.is_empty() {
                return Err(IngestError::RebuildSkipped(skipped).into());
            }
        }
        Ok(satisfied)
    }

    /// Adopt a fetched/settled manifest: persist the blob, hand the new
    /// pointer to the host, refresh our own announcement state.
    fn adopt(&mut self, id: BlobId, bytes: Vec<u8>, man: RootManifest) -> Result<(), SessionError> {
        // Keep the manifest as a stored blob: agreement records may
        // reference it across restarts.
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

// --- shared helpers ---------------------------------------------------------------

fn hex_short(b: &BlobId) -> String {
    hex(b)[..12].to_string()
}

fn wire_store_err(e: ferry_store::store::StoreError) -> ProtoError {
    ProtoError::Io(std::io::Error::other(e.to_string()))
}

/// Lineage comparison: newer creation timestamp wins; device id and root
/// break ties into a total order both peers compute identically.
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

/// Verify a pack's name against its ciphertext, then write it into the
/// store's packs directory atomically. The single receiver-side ingest
/// path shared by the v1 driver, tests, and the engine facade.
pub fn ingest_pack_verified(
    store: &Store,
    claimed_name: &PackId,
    bytes: &[u8],
) -> Result<(), IngestError> {
    let found = ferry_store::pack::pack_name_of(bytes);
    if found != *claimed_name {
        return Err(IngestError::NameMismatch {
            claimed: hex(claimed_name),
            found: hex(&found),
        });
    }
    let dot = store.store_dir();
    ferry_store::pack::write_pack_atomically(&dot.join("tmp"), &dot.join("packs"), bytes)?;
    Ok(())
}

/// Chunk one folder's index rows into `INDEX_ADVERT` frames at the
/// normative cap; at least one advert always goes out, even when empty.
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
