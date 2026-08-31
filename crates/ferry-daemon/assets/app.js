"use strict";



const $ = (id) => document.getElementById(id);
const $$ = (sel) => document.querySelectorAll(sel);


let currentState = "synced";
let lastStatus = null;
let lastManifestId = null;
let soundEnabled = true;
let audioCtx = null;

let sse = null;
let sseErrors = 0;
let sseRetryDelay = 1000;
let sseReconnectTimer = null;
let pollTimer = null;
let sharePollTimer = null;


function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[c]));
}

// ---- Formatting Helpers ---------------------------------------------------
function shortDevice(hex) {
  if (!hex) return "Unknown Device";
  return hex.length > 12 ? hex.slice(0, 6) + "…" + hex.slice(-4) : hex;
}

function friendlyTime(iso) {
  if (!iso) return "recently";
  try {
    const d = new Date(iso);
    return isNaN(d.getTime()) ? iso : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return iso;
  }
}

// ---- Micro-Haptic Audio Synthesizer (Issue 04) -----------------------------
function playHapticFeedback(type = "tick") {
  if (!soundEnabled) return;
  try {
    const AudioContextClass = window.AudioContext || window.webkitAudioContext;
    if (!AudioContextClass) return;
    if (!audioCtx) audioCtx = new AudioContextClass();
    if (audioCtx.state === "suspended") audioCtx.resume();

    const now = audioCtx.currentTime;
    const osc = audioCtx.createOscillator();
    const gain = audioCtx.createGain();
    osc.connect(gain);
    gain.connect(audioCtx.destination);

    if (type === "tick") {
      osc.type = "sine";
      osc.frequency.setValueAtTime(880, now);
      osc.frequency.exponentialRampToValueAtTime(240, now + 0.01);
      gain.gain.setValueAtTime(0.035, now);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.01);
      osc.start(now);
      osc.stop(now + 0.012);
    } else if (type === "snap") {
      osc.type = "triangle";
      osc.frequency.setValueAtTime(1100, now);
      osc.frequency.exponentialRampToValueAtTime(320, now + 0.018);
      gain.gain.setValueAtTime(0.05, now);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.018);
      osc.start(now);
      osc.stop(now + 0.02);
    } else if (type === "success") {
      osc.type = "sine";
      osc.frequency.setValueAtTime(560, now);
      osc.frequency.exponentialRampToValueAtTime(1120, now + 0.035);
      gain.gain.setValueAtTime(0.04, now);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.035);
      osc.start(now);
      osc.stop(now + 0.04);
    } else if (type === "alert") {
      osc.type = "sine";
      osc.frequency.setValueAtTime(360, now);
      osc.frequency.exponentialRampToValueAtTime(180, now + 0.08);
      gain.gain.setValueAtTime(0.05, now);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.08);
      osc.start(now);
      osc.stop(now + 0.09);
    }
  } catch {
    // Graceful fallback for audio limitations
  }
}

// ---- Authentication & Session Management (Issue 02) -----------------------
function getToken() {
  const urlParams = new URLSearchParams(window.location.search);
  const urlToken = urlParams.get("token");
  if (urlToken) {
    sessionStorage.setItem("ferry_token", urlToken);
    // Keep URL clean without page reload
    const cleanUrl = window.location.pathname + window.location.hash;
    window.history.replaceState({}, document.title, cleanUrl);
    return urlToken;
  }
  return sessionStorage.getItem("ferry_token") || null;
}

function setToken(token) {
  if (token) {
    sessionStorage.setItem("ferry_token", token.trim());
  } else {
    sessionStorage.removeItem("ferry_token");
  }
}

function showTokenModal() {
  const modal = $("token-modal");
  if (!modal) return;
  modal.classList.add("open");
  modal.setAttribute("aria-hidden", "false");
  const input = $("token-input");
  if (input) {
    input.value = "";
    input.focus();
  }
}

function hideTokenModal() {
  const modal = $("token-modal");
  if (!modal) return;
  modal.classList.remove("open");
  modal.setAttribute("aria-hidden", "true");
  const err = $("token-error");
  if (err) err.style.display = "none";
}

// ---- API Client -----------------------------------------------------------
async function api(path, body) {
  const token = getToken();
  const headers = {};
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
  }
  if (token) {
    headers["Authorization"] = "Bearer " + token;
  }

  const opts = {
    method: body !== undefined ? "POST" : "GET",
    headers,
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  };

  let res;
  try {
    res = await fetch(path, opts);
  } catch (e) {
    throw { error: "Local daemon connection failed", code: "network", hint: String(e) };
  }

  const text = await res.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    /* non-JSON body */
  }

  if (res.status === 403) {
    showTokenModal();
    throw { error: "Authentication required", code: "forbidden", hint: "Please provide a valid session token." };
  }

  if (!res.ok) {
    if (data && data.error) throw data;
    throw {
      error: res.statusText || "Request failed",
      code: "http-" + res.status,
      hint: text.slice(0, 300) || "(no response body)",
    };
  }

  return data;
}

// ---- Activity Feed Stream -------------------------------------------------
function addActivity(title, timeStr = null) {
  const feed = $("activity-feed");
  if (!feed) return;

  const now = new Date();
  const time = timeStr || now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  const row = document.createElement("div");
  row.className = "flow-row";
  row.innerHTML = `
    <div class="row-left">
      <span class="row-title">${esc(title)}</span>
    </div>
    <span class="row-time">${esc(time)}</span>
  `;

  feed.prepend(row);

  // Retain clean buffer (up to 50 items)
  while (feed.children.length > 50) {
    feed.removeChild(feed.lastChild);
  }
}

// ---- Connection Indicator -------------------------------------------------
function setConn(mode, label) {
  const beacon = $("conn-beacon");
  const text = $("conn-text");
  if (text) text.textContent = label;
  if (beacon) {
    if (mode === "live") {
      beacon.style.backgroundColor = "var(--state-synced)";
      beacon.style.boxShadow = "0 0 8px var(--state-synced-glow)";
    } else if (mode === "poll") {
      beacon.style.backgroundColor = "var(--state-syncing)";
      beacon.style.boxShadow = "0 0 8px var(--state-syncing-glow)";
    } else if (mode === "off") {
      beacon.style.backgroundColor = "var(--state-offline)";
      beacon.style.boxShadow = "none";
    }
  }
}

// ---- State Morphing & Telemetry Presentation (Issue 02) -------------------
function applyState(mode, status = null) {
  const stateChanged = currentState !== mode;
  currentState = mode;

  const mainCard = $("main-card");
  const heroBeacon = $("hero-beacon");
  const beaconCore = heroBeacon ? heroBeacon.querySelector(".beacon-core") : null;
  const beaconRing = heroBeacon ? heroBeacon.querySelector(".beacon-ring") : null;
  const heroTitle = $("hero-title");
  const heroSub = $("hero-sub");
  const heroStateBadge = $("hero-state-badge");
  const metricHash = $("metric-hash");
  const metricHeld = $("metric-held");
  const metricConflicts = $("metric-conflicts");
  const metricCipher = $("metric-cipher");
  const metricChannel = $("metric-channel");
  const btnPin = $("btn-pin");
  const btnPinLabel = $("btn-pin-label");
  const btnRelease = $("btn-release");
  const syncTrack = $("sync-track");
  const connBeacon = $("conn-beacon");
  const connText = $("conn-text");

  // Emil Kowalski Subtle Blur Morphing on State Change
  if (mainCard && stateChanged) {
    mainCard.classList.add("is-blur-transitioning");
    setTimeout(() => mainCard.classList.remove("is-blur-transitioning"), 180);
  }

  if (heroBeacon) {
    heroBeacon.className = "hero-beacon state-" + mode;
  }

  if (beaconCore) {
    beaconCore.style.backgroundColor = "var(--state-" + mode + ")";
    beaconCore.style.boxShadow = mode === "offline" ? "none" : "0 0 14px var(--state-" + mode + "-glow)";
  }
  if (beaconRing) {
    beaconRing.style.borderColor = "var(--state-" + mode + ")";
  }
  if (connBeacon) {
    connBeacon.style.backgroundColor = "var(--state-" + mode + ")";
    connBeacon.style.boxShadow = mode === "offline" ? "none" : "0 0 8px var(--state-" + mode + "-glow)";
  }

  // Telemetry constants
  if (metricCipher) metricCipher.textContent = "Age-X25519";
  if (metricChannel) metricChannel.textContent = "QUIC";

  const peers = status && status.peers ? status.peers : [];
  const heldCount = status && status.held_changes != null ? status.held_changes : 0;
  const conflictsCount = status && status.conflicts != null ? status.conflicts : 0;

  if (mode === "synced") {
    if (connText) connText.textContent = "Active Session";
    if (heroTitle) heroTitle.textContent = "Synced";
    if (heroStateBadge) {
      heroStateBadge.textContent = "Active";
      heroStateBadge.className = "state-badge";
    }
    if (heroSub) {
      heroSub.textContent = peers.length === 0
        ? "All folders match across peers. Continuous cryptographic verification active."
        : `All files up to date with ${peers.length === 1 ? "1 device" : peers.length + " devices"}. Continuous verification active.`;
    }

    if (btnPinLabel) btnPinLabel.textContent = "Hold Edits";
    if (btnPin) btnPin.style.display = "inline-flex";
    if (btnRelease) btnRelease.style.display = "none";
  } else if (mode === "syncing") {
    if (connText) connText.textContent = "Syncing Delta";
    if (heroTitle) heroTitle.textContent = "Syncing…";
    if (heroStateBadge) {
      const pendingCount = peers.filter((p) => !p.last_agreed_manifest_id || p.last_agreed_manifest_id !== (status && status.manifest_id)).length;
      heroStateBadge.textContent = pendingCount > 0 ? `${pendingCount} Pending` : "Syncing";
      heroStateBadge.className = "state-badge";
    }
    if (heroSub) {
      heroSub.textContent = `Synchronizing folder changes with ${peers.length === 1 ? "1 device" : peers.length + " devices"} over encrypted QUIC stream.`;
    }
  } else if (mode === "holding") {
    if (connText) connText.textContent = "Holding Buffer";
    if (heroTitle) heroTitle.textContent = "Holding";
    if (heroStateBadge) {
      heroStateBadge.textContent = heldCount > 0 ? `${heldCount} Changes Held` : "Protected";
      heroStateBadge.className = "state-badge badge-amber";
    }
    if (heroSub) {
      heroSub.textContent = "Incoming remote edits safely buffered while local agent executes.";
    }

    if (btnPinLabel) btnPinLabel.textContent = "Stop Hold";
    if (btnRelease) btnRelease.style.display = "inline-flex";
  } else if (mode === "conflict") {
    if (connText) connText.textContent = "Conflict Alert";
    if (heroTitle) heroTitle.textContent = conflictsCount === 1 ? "1 Conflict" : `${conflictsCount} Conflicts`;
    if (heroStateBadge) {
      heroStateBadge.textContent = "Quarantined";
      heroStateBadge.className = "state-badge badge-red";
    }
    if (heroSub) {
      heroSub.textContent = conflictsCount === 1
        ? "Conflicting edit safely quarantined. No local files overwritten."
        : `${conflictsCount} conflicting edits safely quarantined. No local files overwritten.`;
    }
  } else if (mode === "offline") {
    if (connText) connText.textContent = "Offline";
    if (heroTitle) heroTitle.textContent = "Offline";
    if (heroStateBadge) {
      heroStateBadge.textContent = "Disconnected";
      heroStateBadge.className = "state-badge badge-dim";
    }
    if (heroSub) {
      heroSub.textContent = "Ferry background daemon not running. Local store in idle mode.";
    }

    if (btnPinLabel) btnPinLabel.textContent = "Hold Edits";
    if (btnRelease) btnRelease.style.display = "none";
  }

  // Update Hairline Telemetry Values
  if (metricHash) {
    if (status && status.manifest_id) {
      const fullHash = status.manifest_id;
      metricHash.textContent = fullHash.slice(0, 8);
      metricHash.title = fullHash;
    } else {
      metricHash.textContent = mode === "offline" ? "--------" : "—";
      metricHash.title = "";
    }
  }

  if (metricHeld) metricHeld.textContent = String(heldCount);
  if (metricConflicts) metricConflicts.textContent = String(conflictsCount);
}

// ---- Render Connected Devices & Fleet --------------------------------------
function renderConnectedDevices(peers, localManifestId) {
  const fleetList = $("fleet-list");
  if (!fleetList) return;

  if (!peers || peers.length === 0) {
    fleetList.innerHTML = `
      <div class="flow-row" style="color: var(--text-3); font-size: 11.5px; justify-content: center; padding: 12px 8px;">
        <span>No connected peers</span>
      </div>
    `;
    return;
  }

  fleetList.innerHTML = peers.map((p) => {
    const isOnline = p.connectivity === "reachable";
    const isAgreed = Boolean(p.last_agreed_manifest_id && p.last_agreed_manifest_id === localManifestId);
    const dotClass = isAgreed ? "peer-synced" : (isOnline ? "peer-syncing" : "peer-offline");
    const name = "Device " + shortDevice(p.device_id);
    const lastSeen = p.agreed_at ? "Last synced " + friendlyTime(p.agreed_at) : (isOnline ? "Online now" : "Offline");
    const transport = "QUIC";
    const statusLabel = isAgreed ? "In Sync ✓" : (isOnline ? "Syncing…" : "Offline");
    const badgeColor = isAgreed
      ? "color: var(--state-synced); background: rgba(48,209,88,0.12);"
      : (isOnline ? "color: var(--state-syncing); background: rgba(14,165,233,0.12);" : "color: var(--text-3); background: rgba(255,255,255,0.05);");

    return `
      <div class="flow-row">
        <div class="row-left">
          <span class="peer-dot ${dotClass}"></span>
          <div style="display: flex; flex-direction: column; gap: 1px;">
            <span class="row-title">${esc(name)}</span>
            <span class="row-subtitle font-mono">${esc(transport)} · ${esc(lastSeen)}</span>
          </div>
        </div>
        <span class="state-badge" style="font-size: 10px; padding: 2px 7px; ${badgeColor}">${esc(statusLabel)}</span>
      </div>
    `;
  }).join("");
}

// ---- Render Discovered Nearby Devices --------------------------------------
function renderDiscoveredDevices(devices, peers) {
  const list = $("discovered-list");
  const badge = $("discovered-badge");
  if (!list) return;

  const connectedSet = new Set((peers || []).map((p) => p.device_id));
  const unlinked = (devices || []).filter((d) => !connectedSet.has(d.device_id));

  if (badge) {
    badge.textContent = `${unlinked.length} nearby`;
  }

  if (unlinked.length === 0) {
    list.innerHTML = `
      <div class="flow-row" style="color: var(--text-3); font-size: 11.5px; justify-content: center; padding: 12px 8px;">
        <span>No nearby unlinked devices detected</span>
      </div>
    `;
    return;
  }

  list.innerHTML = unlinked.map((d) => {
    const name = "Device " + shortDevice(d.device_id);
    const addr = d.address || "Local Network (mDNS)";
    return `
      <div class="flow-row">
        <div class="row-left">
          <span class="peer-dot" style="background-color: var(--amber-warn); box-shadow: 0 0 6px rgba(245,158,11,0.4);"></span>
          <div style="display: flex; flex-direction: column; gap: 1px;">
            <span class="row-title">${esc(name)}</span>
            <span class="row-subtitle font-mono">${esc(addr)}</span>
          </div>
        </div>
        <button class="btn btn-ghost btn-sm btn-pair-device" data-device-id="${esc(d.device_id)}" type="button" style="font-size: 11px; padding: 2px 8px; color: var(--state-synced); border-color: rgba(48,209,88,0.3);">
          + Pair
        </button>
      </div>
    `;
  }).join("");

  list.querySelectorAll(".btn-pair-device").forEach((btn) => {
    btn.addEventListener("click", () => {
      const devId = btn.getAttribute("data-device-id");
      if (devId) {
        pairDiscoveredDevice(devId);
      }
    });
  });
}

// ---- Render Status Document -----------------------------------------------
function renderStatus(s) {
  lastStatus = s;
  const statusErr = $("status-error");
  if (statusErr) statusErr.style.display = "none";

  // Check manifest update
  if (lastManifestId && s.manifest_id && lastManifestId !== s.manifest_id) {
    addActivity("Manifest updated: changes synchronized");
    playHapticFeedback("tick");
  }
  lastManifestId = s.manifest_id;

  // Determine state
  let mode = "synced";
  const conflictsCount = s.conflicts || 0;
  const pin = s.pin || { state: "none", holding: false };
  const peers = s.peers || [];

  if (conflictsCount > 0) {
    mode = "conflict";
  } else if (pin.holding || pin.state === "active") {
    mode = "holding";
  } else if (peers.length > 0 && peers.some((p) => !p.last_agreed_manifest_id || p.last_agreed_manifest_id !== s.manifest_id)) {
    mode = "syncing";
  } else {
    mode = "synced";
  }

  applyState(mode, s);
  renderConnectedDevices(peers, s.manifest_id);
  renderDiscoveredDevices(s.discovered_devices || [], peers);
}

// ---- Load Status & Conflicts ----------------------------------------------
async function loadStatus() {
  try {
    const s = await api("/api/status");
    renderStatus(s);
    setConn("live", "Active Session");
    return true;
  } catch (err) {
    if (err && err.code === "forbidden") {
      return false;
    }
    applyState("offline", null);
    setConn("off", "Offline");
    renderConnectedDevices([], null);
    return false;
  }
}

async function loadConflicts() {
  try {
    const doc = await api("/api/conflicts");
    if (doc && doc.entries && doc.entries.length > 0) {
      const conflictsCount = doc.entries.length;
      if (lastStatus) {
        lastStatus.conflicts = conflictsCount;
        renderStatus(lastStatus);
      }
    }
  } catch {
    // ignore
  }
}

// ---- SSE Streaming & Resilient Polling Fallback (Issue 02) -----------------
function startPolling() {
  if (pollTimer) return;
  setConn("poll", "Polling (1.5s)");
  pollTimer = setInterval(async () => {
    try {
      await loadStatus();
    } catch {
      // silent catch for resilient polling
    }
  }, 1500);
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

function startEvents() {
  stopPolling();
  if (sse) {
    try { sse.close(); } catch {}
    sse = null;
  }

  const token = getToken();
  const url = token ? "/api/events?token=" + encodeURIComponent(token) : "/api/events";

  try {
    sse = new EventSource(url);
  } catch {
    startPolling();
    return;
  }

  sse.addEventListener("state", (ev) => {
    sseErrors = 0;
    sseRetryDelay = 1000;
    setConn("live", "Active Session");
    try {
      const data = JSON.parse(ev.data);
      if (data && data.command === "status") {
        renderStatus(data);
      }
    } catch {
      // ignore
    }
  });

  sse.onopen = () => {
    sseErrors = 0;
    sseRetryDelay = 1000;
    setConn("live", "Active Session");
  };

  sse.onerror = () => {
    if (sse) {
      try { sse.close(); } catch {}
      sse = null;
    }
    sseErrors += 1;
    startPolling();

    // Reconnection exponential backoff
    if (!sseReconnectTimer) {
      sseReconnectTimer = setTimeout(() => {
        sseReconnectTimer = null;
        sseRetryDelay = Math.min(sseRetryDelay * 2, 10000);
        startEvents();
      }, sseRetryDelay);
    }
  };
}

// ---- Actions: Instant Sync (Issue 03) --------------------------------------
async function doSync() {
  playHapticFeedback("tick");
  const syncTrack = $("sync-track");
  if (syncTrack) syncTrack.classList.add("visible");
  addActivity("Sync Triggered");

  try {
    await loadStatus();
    await loadConflicts();
    playHapticFeedback("success");
    addActivity("Sync Completed");
  } catch (err) {
    if (err && err.code === "forbidden") return;
    playHapticFeedback("alert");
    addActivity("Sync Failed: " + (err.error || err.code || "Network error"));
  } finally {
    setTimeout(() => {
      if (syncTrack) syncTrack.classList.remove("visible");
    }, 600);
  }
}

// ---- Actions: Work Protection / Pinning (Issue 03) -------------------------
async function doPin(action) {
  try {
    const paths = action === "start" ? ["*"] : null;
    let doc;
    if (action === "release") {
      try {
        doc = await api("/api/pin/release", { folder: null });
      } catch {
        doc = await api("/api/pin/stop", { folder: null });
      }
    } else {
      doc = await api("/api/pin/" + action, { folder: null, paths });
    }

    if (action === "start") {
      playHapticFeedback("snap");
      addActivity("Work Protection Activated");
    } else if (action === "stop") {
      playHapticFeedback("success");
      addActivity("Work Protection Stopped");
    } else {
      playHapticFeedback("success");
      addActivity("Held Edits Released & Merged");
    }
    await loadStatus();
    await loadConflicts();
  } catch (err) {
    if (err && err.code === "forbidden") return;
    playHapticFeedback("alert");
    addActivity("Pin Error: " + (err.error || err.code || "Unknown error"));
  }
}

function togglePin() {
  const isHoldingOrActive = Boolean(
    lastStatus && lastStatus.pin && (lastStatus.pin.holding || lastStatus.pin.state === "active")
  );
  if (isHoldingOrActive) {
    doPin("stop");
  } else {
    doPin("start");
  }
}

// ---- Actions: Pairing & Offer Creation (Issue 03) --------------------------
function stopSharePolling() {
  if (sharePollTimer) {
    clearInterval(sharePollTimer);
    sharePollTimer = null;
  }
}

function openPairModal() {
  playHapticFeedback("tick");
  const modal = $("pair-modal");
  if (!modal) return;
  modal.classList.add("open");
  modal.setAttribute("aria-hidden", "false");
}

function closePairModal() {
  playHapticFeedback("tick");
  const modal = $("pair-modal");
  if (!modal) return;
  modal.classList.remove("open");
  modal.setAttribute("aria-hidden", "true");
  stopSharePolling();
}

async function doCreateOffer(iKnow = false) {
  stopSharePolling();
  playHapticFeedback("snap");

  const warnEl = $("share-warn");
  const resEl = $("share-result");
  const anywayBtn = $("share-anyway");
  const offerBox = $("offer-box");
  const tokenDisplay = $("token-display");
  const qrDisplay = $("share-qr-display");
  const statusText = $("share-status-text");

  if (warnEl) warnEl.style.display = "none";
  if (resEl) resEl.style.display = "none";
  if (anywayBtn) anywayBtn.style.display = "none";

  addActivity("Creating Pairing Offer…");

  try {
    let doc;
    try {
      doc = await api("/api/share", { folder: null, i_know: iKnow });
    } catch (e) {
      if (e && (e.code === "http-404" || e.code === "not-found")) {
        doc = await api("/api/pair/share", { folder: null, i_know: iKnow });
      } else {
        throw e;
      }
    }

    const code = doc.short_code || doc.code || doc.token || "";
    const payload = code || doc.offer_file || JSON.stringify(doc);
    if (tokenDisplay) tokenDisplay.value = payload;
    if (qrDisplay) qrDisplay.textContent = doc.qr_code || "";
    if (statusText) statusText.textContent = "Waiting for peer device to enter code…";
    if (offerBox) offerBox.style.display = "block";
    if (resEl) {
      resEl.style.display = "block";
      resEl.textContent = `Pairing code generated: ${code || "Active"}. Waiting for peer.`;
    }
    playHapticFeedback("success");
    addActivity("Pairing Code Created: " + (code || "Active"));

    // Poll share status until connected
    sharePollTimer = setInterval(async () => {
      try {
        const s = await api("/api/share/status");
        if (s && (s.status === "completed" || s.status === "paired")) {
          stopSharePolling();
          if (resEl) {
            resEl.textContent = "Pairing completed! Peer device connected.";
          }
          if (statusText) {
            statusText.textContent = "Pairing completed! Peer device connected.";
          }
          playHapticFeedback("success");
          addActivity("Pairing Completed: Connected Peer");
          loadStatus();
        }
      } catch {
        // ignore polling errors
      }
    }, 1500);
  } catch (err) {
    if (err && err.code === "secrets-found") {
      if (warnEl) {
        warnEl.style.display = "block";
        warnEl.textContent = err.error || "Sensitive secrets detected in folder.";
      }
      if (anywayBtn) anywayBtn.style.display = "block";
      playHapticFeedback("alert");
      addActivity("Pairing Blocked: Secrets Detected");
    } else {
      if (warnEl) {
        warnEl.style.display = "block";
        warnEl.textContent = err.error || "Failed to create pairing token.";
      }
      playHapticFeedback("alert");
      addActivity("Pairing Offer Failed: " + (err.error || err.code));
    }
  }
}

async function pairDiscoveredDevice(devId) {
  openPairModal();
  stopSharePolling();
  playHapticFeedback("snap");
  addActivity("Initiating Pairing with " + shortDevice(devId) + "…");

  const warnEl = $("share-warn");
  const resEl = $("share-result");
  const offerBox = $("offer-box");
  const tokenDisplay = $("token-display");
  const qrDisplay = $("share-qr-display");
  const statusText = $("share-status-text");

  if (warnEl) warnEl.style.display = "none";
  if (resEl) resEl.style.display = "none";

  try {
    const doc = await api("/api/pair/device", { device_id: devId });
    const code = doc.short_code || doc.code || "";
    if (tokenDisplay) tokenDisplay.value = code;
    if (qrDisplay) qrDisplay.textContent = doc.qr_code || "";
    if (statusText) statusText.textContent = "Pairing handshake initiated with " + shortDevice(devId) + "…";
    if (offerBox) offerBox.style.display = "block";
    if (resEl) {
      resEl.style.display = "block";
      resEl.textContent = `Pairing code generated (${code}). Waiting for peer handshake.`;
    }
    playHapticFeedback("success");
    addActivity("Pairing Offer Created for " + shortDevice(devId));

    sharePollTimer = setInterval(async () => {
      try {
        const s = await api("/api/share/status");
        if (s && (s.status === "completed" || s.status === "paired")) {
          stopSharePolling();
          if (statusText) statusText.textContent = "Pairing completed! Peer device connected.";
          if (resEl) resEl.textContent = "Pairing completed! Peer device connected.";
          playHapticFeedback("success");
          addActivity("Pairing Completed with " + shortDevice(devId));
          loadStatus();
        }
      } catch {
        // ignore polling errors
      }
    }, 1500);
  } catch (err) {
    if (warnEl) {
      warnEl.style.display = "block";
      warnEl.textContent = err.error || "Failed to initiate pairing with device.";
    }
    playHapticFeedback("alert");
    addActivity("Pairing Failed: " + (err.error || err.code));
  }
}

async function copyToken() {
  const tokenDisplay = $("token-display");
  const tokenVal = tokenDisplay ? tokenDisplay.value : "";
  if (!tokenVal) return;
  try {
    await navigator.clipboard.writeText(tokenVal);
  } catch {
    // fallback
  }
  const btn = $("btn-copy-token");
  if (btn) {
    btn.textContent = "Copied!";
    playHapticFeedback("success");
    setTimeout(() => {
      btn.textContent = "Copy";
    }, 1500);
  }
}

async function doAcceptPair() {
  playHapticFeedback("snap");
  const input = $("accept-input");
  const destInput = $("join-dest-input");
  const val = input ? input.value.trim() : "";
  const destVal = destInput ? destInput.value.trim() : "";
  const resEl = $("pair-result");
  const errEl = $("pair-err");

  if (resEl) resEl.style.display = "none";
  if (errEl) errEl.style.display = "none";

  if (!val) {
    if (errEl) {
      errEl.style.display = "block";
      errEl.textContent = "Please enter a 6-character pairing code or offer file path.";
    }
    playHapticFeedback("alert");
    return;
  }

  addActivity("Joining Remote Folder…");

  try {
    let doc;
    if (destVal) {
      doc = await api("/api/pair/join", { code: val, target_dir: destVal });
    } else {
      doc = await api("/api/pair/accept", { code_or_payload: val });
    }
    if (resEl) {
      resEl.style.display = "block";
      resEl.textContent = "Joined folder! Connected to " + (doc.folder || "folder");
    }
    if (input) input.value = "";
    if (destInput) destInput.value = "";
    playHapticFeedback("success");
    addActivity("Folder Joined: " + (doc.folder || "folder"));
    await loadStatus();
    setTimeout(() => {
      closePairModal();
    }, 1200);
  } catch (err) {
    if (errEl) {
      errEl.style.display = "block";
      errEl.textContent = err.error || "Failed to join remote folder.";
    }
    playHapticFeedback("alert");
    addActivity("Pair Join Failed: " + (err.error || err.code));
  }
}

// ---- Folder Picker Modal (Issue 06) -----------------------------------------
let pickerCurrentPath = "";
let pickerDebounceTimer = null;

async function fetchFsList(path) {
  const qs = path ? "?path=" + encodeURIComponent(path) : "";
  return api("/api/fs/ls" + qs);
}

function renderBreadcrumb(absPath) {
  const bc = $("picker-breadcrumb");
  if (!bc) return;
  bc.innerHTML = "";
  const parts = absPath.split("/").filter(Boolean);
  const makeCrumb = (label, target) => {
    const span = document.createElement("span");
    span.className = "crumb";
    span.textContent = label;
    span.addEventListener("click", () => loadPickerPath(target));
    return span;
  };
  const sep = () => {
    const s = document.createElement("span");
    s.className = "crumb-sep";
    s.textContent = "/";
    return s;
  };
  bc.appendChild(makeCrumb("/", "/"));
  let accum = "";
  parts.forEach((p) => {
    bc.appendChild(sep());
    accum += "/" + p;
    bc.appendChild(makeCrumb(p, accum));
  });
}

function renderEntries(entries, absPath) {
  const container = $("picker-entries");
  if (!container) return;
  container.innerHTML = "";
  if (!entries || entries.length === 0) {
    container.innerHTML = '<div style="color: var(--text-3); font-size: 11.5px; padding: 8px; text-align: center;">Empty folder</div>';
    return;
  }
  entries.forEach((e) => {
    const row = document.createElement("div");
    row.className = "picker-row" + (e.is_dir ? " is-dir" : "");
    const meta = e.is_dir ? "dir" : "file";
    const gitMark = e.is_git_repo ? " · git" : "";
    const syncedMark = e.is_already_synced ? " · synced" : "";
    row.innerHTML = '<span class="row-name">' + esc(e.name) + '</span><span class="row-meta">' + esc(meta + gitMark + syncedMark) + '</span>';
    row.addEventListener("click", () => {
      if (e.is_dir) {
        loadPickerPath(e.path);
      } else {
        const input = $("picker-path-input");
        if (input) input.value = e.path;
      }
    });
    container.appendChild(row);
  });
}

async function loadPickerPath(path) {
  const input = $("picker-path-input");
  const warn = $("picker-warn");
  if (warn) warn.style.display = "none";
  try {
    const doc = await fetchFsList(path);
    pickerCurrentPath = doc.absolute_path || path || "";
    if (input) input.value = pickerCurrentPath;
    renderBreadcrumb(pickerCurrentPath);
    renderEntries(doc.entries || [], pickerCurrentPath);
    const sugg = $("picker-suggestions");
    if (sugg) sugg.style.display = "none";
  } catch (err) {
    if (warn) {
      warn.style.display = "block";
      warn.textContent = (err && err.error) ? err.error : "Failed to list folder";
    }
  }
}

function presetPath(name) {
  if (name === "home") return null;
  if (name === "projects") return "/projects";
  if (name === "desktop") return "/Desktop";
  return null;
}

async function handlePresetClick(name) {
  playHapticFeedback("tick");
  const p = presetPath(name);
  if (p === null) {
    await loadPickerPath(null);
  } else {
    await loadPickerPath(p);
  }
}

function scheduleAutocomplete() {
  if (pickerDebounceTimer) clearTimeout(pickerDebounceTimer);
  pickerDebounceTimer = setTimeout(async () => {
    const input = $("picker-path-input");
    const sugg = $("picker-suggestions");
    if (!input || !sugg) return;
    const val = input.value.trim();
    if (!val) { sugg.style.display = "none"; return; }
    const lastSlash = val.lastIndexOf("/");
    const dirPart = lastSlash >= 0 ? val.slice(0, lastSlash) || "/" : "";
    const prefix = lastSlash >= 0 ? val.slice(lastSlash + 1) : val;
    if (!prefix) { sugg.style.display = "none"; return; }
    try {
      const doc = await fetchFsList(dirPart || null);
      const matches = (doc.entries || []).filter((e) => e.name.toLowerCase().startsWith(prefix.toLowerCase()));
      if (matches.length === 0) { sugg.style.display = "none"; return; }
      sugg.innerHTML = "";
      matches.slice(0, 8).forEach((m) => {
        const div = document.createElement("div");
        div.className = "suggestion";
        div.textContent = m.name;
        div.addEventListener("click", () => {
          const newPath = (dirPart === "/" ? "/" : dirPart + "/") + m.name;
          input.value = newPath;
          sugg.style.display = "none";
          loadPickerPath(newPath);
        });
        sugg.appendChild(div);
      });
      sugg.style.display = "flex";
    } catch {
      sugg.style.display = "none";
    }
  }, 150);
}

function openFolderPicker() {
  playHapticFeedback("tick");
  const modal = $("folder-picker-modal");
  if (!modal) return;
  modal.classList.add("open");
  modal.setAttribute("aria-hidden", "false");
  const warn = $("picker-warn");
  if (warn) warn.style.display = "none";
  const sugg = $("picker-suggestions");
  if (sugg) sugg.style.display = "none";
  loadPickerPath(pickerCurrentPath || null);
  const input = $("picker-path-input");
  if (input) setTimeout(() => input.focus(), 50);
}

function closeFolderPicker() {
  playHapticFeedback("tick");
  const modal = $("folder-picker-modal");
  if (!modal) return;
  modal.classList.remove("open");
  modal.setAttribute("aria-hidden", "true");
  const sugg = $("picker-suggestions");
  if (sugg) sugg.style.display = "none";
  if (pickerDebounceTimer) clearTimeout(pickerDebounceTimer);
}

async function doRegisterFolder(force) {
  const input = $("picker-path-input");
  const warn = $("picker-warn");
  const raw = input ? input.value.trim() : pickerCurrentPath;
  if (!raw) return;
  if (warn) warn.style.display = "none";
  try {
    const doc = await api("/api/registry/register", { path: raw, force: !!force });
    if (warn) warn.style.display = "none";
    closeFolderPicker();
    addActivity("Folder registered: " + raw);
    playHapticFeedback("success");
    await loadStatus();
    return doc;
  } catch (err) {
    if (err && (err.code === "not-initialized" || err.code === "not_initialized")) {
      if (warn) {
        warn.style.display = "block";
        warn.textContent = (err.error || "Not an initialized Ferry folder") +
          " — " + (err.hint || "run `ferry init` or `ferry pair` first");
      }
      playHapticFeedback("alert");
      addActivity("Folder register blocked: not initialized");
      return;
    }
    if (err && (err.code === "secrets-found" || err.code === "secrets_found")) {
      if (warn) {
        warn.style.display = "block";
        warn.textContent = err.error || "Secrets detected in folder. Review before sharing.";
        warn.innerHTML += ' <button id="picker-share-anyway" class="btn btn-danger btn-sm" type="button" style="margin-left: 8px;">Share Anyway</button>';
        const btn = warn.querySelector("#picker-share-anyway");
        if (btn) btn.addEventListener("click", () => doRegisterFolder(true));
      }
      playHapticFeedback("alert");
      addActivity("Folder register blocked: secrets found");
      return;
    }
    if (warn) {
      warn.style.display = "block";
      warn.textContent = (err && err.error) ? err.error : "Failed to register folder";
    }
    playHapticFeedback("alert");
  }
}

// ---- Theme Controller (Issue 04) -------------------------------------------
function updateThemeIcons(theme) {
  const moon = $("icon-theme-moon");
  const sun = $("icon-theme-sun");
  if (!moon || !sun) return;
  if (theme === "dark") {
    moon.style.display = "block";
    sun.style.display = "none";
  } else {
    moon.style.display = "none";
    sun.style.display = "block";
  }
}

function initTheme() {
  const savedTheme = localStorage.getItem("ferry_theme") || "dark";
  document.documentElement.setAttribute("data-theme", savedTheme);
  updateThemeIcons(savedTheme);

  const savedSound = localStorage.getItem("ferry_sound");
  soundEnabled = savedSound !== "false";
  updateSoundIcons();
}

function toggleTheme() {
  playHapticFeedback("tick");
  const cur = document.documentElement.getAttribute("data-theme") || "dark";
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", next);
  localStorage.setItem("ferry_theme", next);
  updateThemeIcons(next);
  addActivity(`Theme switched to ${next} mode`);
}

function updateSoundIcons() {
  const onIcon = $("icon-sound-on");
  const offIcon = $("icon-sound-off");
  if (!onIcon || !offIcon) return;
  if (soundEnabled) {
    onIcon.style.display = "block";
    offIcon.style.display = "none";
  } else {
    onIcon.style.display = "none";
    offIcon.style.display = "block";
  }
}

function toggleSound() {
  soundEnabled = !soundEnabled;
  localStorage.setItem("ferry_sound", soundEnabled ? "true" : "false");
  updateSoundIcons();
  if (soundEnabled) playHapticFeedback("success");
  addActivity(soundEnabled ? "Micro-Haptics Enabled" : "Micro-Haptics Muted");
}

// ---- Event Listeners & Initialization -------------------------------------
function setupEventListeners() {
  // Theme & Audio Controls
  const btnTheme = $("btn-theme");
  if (btnTheme) btnTheme.addEventListener("click", toggleTheme);

  const btnSound = $("btn-sound");
  if (btnSound) btnSound.addEventListener("click", toggleSound);

  // Actions
  const btnSync = $("btn-sync");
  if (btnSync) btnSync.addEventListener("click", doSync);

  const btnPin = $("btn-pin");
  if (btnPin) btnPin.addEventListener("click", togglePin);

  const btnRelease = $("btn-release");
  if (btnRelease) btnRelease.addEventListener("click", () => doPin("release"));

  // Modals & Pairing
  const btnPair = $("btn-pair");
  if (btnPair) btnPair.addEventListener("click", openPairModal);

  const btnCloseModal = $("btn-close-modal");
  if (btnCloseModal) btnCloseModal.addEventListener("click", closePairModal);

  const pairModal = $("pair-modal");
  if (pairModal) {
    pairModal.addEventListener("click", (e) => {
      if (e.target === pairModal) closePairModal();
    });
  }

  const btnCreateOffer = $("btn-create-offer");
  if (btnCreateOffer) btnCreateOffer.addEventListener("click", () => doCreateOffer(false));

  const shareAnyway = $("share-anyway");
  if (shareAnyway) shareAnyway.addEventListener("click", () => doCreateOffer(true));

  const btnCopyToken = $("btn-copy-token");
  if (btnCopyToken) btnCopyToken.addEventListener("click", copyToken);

  const btnAccept = $("btn-accept");
  if (btnAccept) btnAccept.addEventListener("click", doAcceptPair);

  // Activity feed clear
  const btnClear = $("btn-clear");
  if (btnClear) {
    btnClear.addEventListener("click", () => {
      playHapticFeedback("tick");
      const feed = $("activity-feed");
      if (feed) feed.innerHTML = "";
    });
  }

  // Token Authentication Form
  const tokenForm = $("token-form");
  if (tokenForm) {
    tokenForm.addEventListener("submit", async (e) => {
      e.preventDefault();
      const input = $("token-input");
      const tokenVal = input ? input.value.trim() : "";
      if (!tokenVal) return;

      setToken(tokenVal);
      try {
        const ok = await loadStatus();
        if (ok) {
          hideTokenModal();
          playHapticFeedback("success");
          addActivity("Session Authenticated");
          startEvents();
          loadConflicts();
        } else {
          const err = $("token-error");
          if (err) {
            err.style.display = "block";
            err.textContent = "Invalid token. Please re-enter.";
          }
          playHapticFeedback("alert");
        }
      } catch {
        const err = $("token-error");
        if (err) {
          err.style.display = "block";
          err.textContent = "Invalid token. Access denied.";
        }
        playHapticFeedback("alert");
      }
    });
  }

  const btnCloseTokenModal = $("btn-close-token-modal");
  if (btnCloseTokenModal) btnCloseTokenModal.addEventListener("click", hideTokenModal);

  // Folder Picker Modal
  const btnAddFolder = $("btn-add-folder");
  if (btnAddFolder) btnAddFolder.addEventListener("click", openFolderPicker);

  const btnClosePicker = $("btn-close-picker");
  if (btnClosePicker) btnClosePicker.addEventListener("click", closeFolderPicker);

  const pickerCancel = $("picker-cancel");
  if (pickerCancel) pickerCancel.addEventListener("click", closeFolderPicker);

  const pickerModal = $("folder-picker-modal");
  if (pickerModal) {
    pickerModal.addEventListener("click", (e) => {
      if (e.target === pickerModal) closeFolderPicker();
    });
  }

  const pickerPresets = $("picker-presets");
  if (pickerPresets) {
    pickerPresets.querySelectorAll("[data-preset]").forEach((btn) => {
      btn.addEventListener("click", () => handlePresetClick(btn.getAttribute("data-preset")));
    });
  }

  const pickerInput = $("picker-path-input");
  if (pickerInput) {
    pickerInput.addEventListener("input", scheduleAutocomplete);
    pickerInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        loadPickerPath(pickerInput.value.trim());
      }
    });
  }

  const pickerSelect = $("picker-select");
  if (pickerSelect) pickerSelect.addEventListener("click", () => doRegisterFolder(false));

  // Keyboard Shortcuts (Issue 04)
  window.addEventListener("keydown", (e) => {
    if (["INPUT", "TEXTAREA", "SELECT"].includes(document.activeElement.tagName)) {
      if (e.key === "Escape") {
        document.activeElement.blur();
        closePairModal();
        closeFolderPicker();
      }
      return;
    }

    if (e.key === "t" || e.key === "T") {
      toggleTheme();
    } else if (e.key === "p" || e.key === "P") {
      const modal = $("pair-modal");
      if (modal && (modal.classList.contains("open") || modal.getAttribute("aria-hidden") === "false")) {
        closePairModal();
      } else {
        openPairModal();
      }
    } else if (e.code === "Space") {
      e.preventDefault();
      const syncBtn = $("btn-sync");
      if (syncBtn) syncBtn.click();
    } else if (e.key === "Escape") {
      closePairModal();
      closeFolderPicker();
    }
  });
}

function init() {
  initTheme();
  setupEventListeners();

  const token = getToken();
  if (token) {
    setToken(token);
  }

  addActivity("Dashboard Initialized");

  loadStatus().then((ok) => {
    if (ok) {
      startEvents();
      loadConflicts();
    } else {
      startPolling();
    }
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
