"use strict";

const $ = (id) => document.getElementById(id);

let sse = null;
let sseErrors = 0;
let pollTimer = null;
let lastStatus = null;

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

async function api(path, body) {
  const opts = body !== undefined
    ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }
    : {};
  let res;
  try {
    res = await fetch(path, opts);
  } catch (e) {
    throw { error: "network error", code: "network", hint: String(e) };
  }
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { /* non-JSON body */ }
  if (!res.ok) {
    if (data && data.error) throw data;
    throw {
      error: res.statusText || "request failed",
      code: "http-" + res.status,
      hint: text.slice(0, 300) || "(no response body)",
    };
  }
  return data;
}

function showErr(el, err) {
  el.hidden = false;
  let html = "<b>" + esc(err.code || "error") + "</b> " + esc(err.error || "");
  if (err.hint) html += '<div class="dim" style="color:inherit;opacity:.75">' + esc(err.hint) + "</div>";
  el.innerHTML = html;
}

function hideAll(...els) { els.forEach((e) => { e.hidden = true; }); }

function shortHex(h) {
  return (h && h.length > 16) ? h.slice(0, 8) + "…" + h.slice(-6) : (h || "—");
}

function fmtBytes(n) {
  if (!Number.isFinite(n)) return String(n);
  for (const unit of ["B", "KiB", "MiB", "GiB", "TiB"]) {
    if (n < 1024) return n.toFixed(n < 10 && unit !== "B" ? 1 : 0) + " " + unit;
    n /= 1024;
  }
  return n.toFixed(1) + " PiB";
}

// ---- connection indicator ------------------------------------------------

function setConn(mode, label) {
  const c = $("conn");
  c.className = "conn conn-" + mode;
  $("conn-label").textContent = label;
}

// ---- status --------------------------------------------------------------

async function loadStatus() {
  try {
    const s = await api("/api/status");
    lastStatus = s;
    renderStatus(s);
    hideAll($("status-error"));
  } catch (err) {
    if (err.code === "warming-up") {
      $("overview").hidden = true;
      showErr($("status-error"), { error: "daemon warming up — waiting for first poll tick", code: "warming-up", hint: err.hint });
      return false;
    }
    showErr($("status-error"), err);
    setConn("off", "status unreachable");
    return false;
  }
  return true;
}

function pendingText(v) {
  if (v === null) return "no agreement yet";
  if (v === -1) return "agreement manifest unreadable";
  return String(v);
}

function pinBadge(pin) {
  const b = $("ov-pin");
  b.className = "badge";
  b.textContent = pin.state + (pin.holding ? " · holding" : "");
  if (pin.state === "active") b.classList.add("badge-green");
  else if (pin.state === "stale") b.classList.add("badge-amber");
  else if (pin.state === "released") b.classList.add("badge-blue");
}

function renderStatus(s) {
  $("overview").hidden = false;
  $("ov-device").textContent = s.device_id ?? "—";
  $("ov-folder").textContent = s.folder ?? "—";
  $("ov-folder-id").textContent = s.folder_id ?? "—";
  $("ov-manifest").textContent = s.manifest_id ?? "—";
  $("ov-pending").textContent = pendingText(s.pending_changes);
  $("ov-held").textContent = String(s.held_changes ?? 0);
  const sc = s.scanned || {};
  $("ov-scanned").textContent =
    (sc.files ?? 0) + " files, " + (sc.dirs ?? 0) + " dirs, " +
    (sc.symlinks ?? 0) + " symlinks, " + fmtBytes(sc.bytes_chunked ?? 0);
  pinBadge(s.pin || { state: "none" });

  const peers = s.peers || [];
  $("peers-empty").hidden = peers.length > 0;
  $("peers").innerHTML = peers.map((p) => {
    const agreed = p.last_agreed_manifest_id && p.last_agreed_manifest_id === s.manifest_id;
    const badge = agreed
      ? '<span class="badge badge-green">agreed ✓</span>'
      : '<span class="badge badge-amber">not agreed</span>';
    const sub =
      "last agreed: " + (p.last_agreed_manifest_id ? shortHex(p.last_agreed_manifest_id) : "none") +
      (p.agreed_at ? " · at " + esc(p.agreed_at) : "");
    return '<li><div class="row-head">' +
      '<span class="dot dot-' + esc(p.connectivity || "unknown") + '"></span>' +
      "<code>" + esc(p.device_id) + "</code>" +
      badge +
      '<span class="dim" style="font-size:12px">' + esc(p.connectivity || "unknown") + "</span>" +
      '</div><div class="row-sub">' + sub + "</div></li>";
  }).join("");
}

// ---- conflicts -----------------------------------------------------------

async function loadConflicts() {
  try {
    const doc = await api("/api/conflicts");
    renderConflicts(doc.entries || []);
  } catch (err) {
    $("conflicts").innerHTML = "";
    $("conflicts-empty").hidden = false;
    $("conflicts-empty").innerHTML =
      '<span style="color:var(--red)"><b>' + esc(err.code || "error") + "</b> " +
      esc(err.error || "") + (err.hint ? " — " + esc(err.hint) : "") + "</span>";
  }
}

function renderConflicts(entries) {
  $("conflicts-empty").hidden = entries.length > 0;
  $("conflicts-empty").textContent = "No conflicts recorded — tree is clean.";
  $("conflicts").innerHTML = entries.slice().reverse().map((e) => {
    const q = e.quarantined_as
      ? '<div class="row-sub">quarantined as ' + esc(e.quarantined_as) + "</div>"
      : "";
    return "<li><div class='row-head'><code>" + esc(e.path) + "</code>" +
      '<span class="badge">' + esc(e.kind) + "</span></div>" +
      '<div class="row-sub">' + esc(e.ts) + " · winner " + esc(e.winner?.device ?? "?") +
      " vs loser " + esc(e.loser?.device ?? "?") + "</div>" + q + "</li>";
  }).join("");
}

// ---- SSE with polling fallback --------------------------------------------

function handleStateLine(line) {
  const m = /root=(\S+)\s+agreed=(\S+)/.exec(line || "");
  if (!m) return;
  const [, root, agreed] = m;
  $("live-state").hidden = false;
  $("state-root").textContent = root;
  $("state-agreed").textContent = agreed;
  const badge = $("agree-badge");
  badge.hidden = false;
  if (agreed === "none") {
    badge.className = "badge badge-amber";
    badge.textContent = "agreement: none";
  } else if (agreed === root) {
    badge.className = "badge badge-green";
    badge.textContent = "agreement: in sync";
  } else {
    badge.className = "badge badge-amber";
    badge.textContent = "agreement: diverged";
  }
}

function startPolling() {
  if (pollTimer) return;
  setConn("poll", "polling every 2s (SSE unavailable)");
  pollTimer = setInterval(loadStatus, 2000);
}

function startEvents() {
  try {
    sse = new EventSource("/api/events");
  } catch {
    startPolling();
    return;
  }
  sse.addEventListener("state", (ev) => {
    sseErrors = 0;
    setConn("live", "live via SSE");
    handleStateLine(ev.data);
  });
  // A non-200/non-event-stream reply (e.g. 501) closes the EventSource for
  // good after ONE error — fall back to polling immediately. The counter
  // only covers retryable drops where the source is still CONNECTING.
  sse.onerror = () => {
    if (sse && sse.readyState === EventSource.CLOSED) {
      sse = null;
      startPolling();
      return;
    }
    sseErrors += 1;
    if (sseErrors >= 2) {
      sse.close();
      sse = null;
      startPolling();
    }
  };
}

// ---- actions --------------------------------------------------------------

function parsePaths(raw) {
  const parts = raw.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
  return parts.length ? parts : null;
}

function renderShareResult(el, doc) {
  const warnings = doc.warnings || [];
  let html = "<b>" + esc(doc.command ?? "share") + "</b> payload written:" +
    "<br><code>" + esc(doc.offer_file ?? "?") + "</code>" +
    "<br>peer device: <code>" + esc(doc.peer_device_id ?? "?") + "</code>" +
    "<br>warnings reviewed: " + esc(String(doc.warnings_reviewed ?? false));
  if (warnings.length) {
    html += "<br>warnings carried into the share:";
    html += warningsTable(warnings);
  }
  el.innerHTML = html;
}

function warningsTable(warnings) {
  return "<table><tr><th>path</th><th>line</th><th>class</th><th>preview</th></tr>" +
    warnings.map((w) =>
      "<tr><td>" + esc(w.path) + "</td><td>" + esc(w.line ?? "—") + "</td>" +
      "<td>" + esc(w.class) + "</td><td>" + esc(w.preview) + "</td></tr>"
    ).join("") + "</table>";
}

async function doShare(iKnow) {
  hideAll($("share-warn"), $("share-result"));
  $("share-anyway").hidden = true;
  $("share-btn").disabled = true;
  try {
    const doc = await api("/api/share", { folder: null, i_know: iKnow });
    renderShareResult($("share-result"), doc);
    $("share-result").hidden = false;
  } catch (err) {
    showErr($("share-warn"), err);
    if (Array.isArray(err.warnings) && err.warnings.length) {
      $("share-warn").insertAdjacentHTML("beforeend",
        "<div style='margin-top:4px'>secret findings:</div>" + warningsTable(err.warnings));
    }
    $("share-anyway").hidden = err.code !== "secrets-found";
  } finally {
    $("share-btn").disabled = false;
  }
}

async function doPairAccept(ev) {
  ev.preventDefault();
  hideAll($("pair-err"), $("pair-result"));
  const payloadPath = $("pair-payload").value.trim();
  const dir = $("pair-dir").value.trim() || null;
  if (!payloadPath) {
    showErr($("pair-err"), { error: "payload path is required", code: "validation", hint: "enter the path to the pairing payload file" });
    return;
  }
  try {
    const doc = await api("/api/pair/accept", { payload_path: payloadPath, dir });
    $("pair-result").innerHTML =
      "<b>pair accepted</b><br>folder: <code>" + esc(doc.folder ?? "?") + "</code>" +
      "<br>folder id: <code>" + esc(doc.folder_id ?? "?") + "</code>" +
      "<br>this device: <code>" + esc(doc.device_id ?? "?") + "</code>" +
      "<br>expected short code: <code>" + esc(doc.expected_short_code ?? "?") + "</code>";
    $("pair-result").hidden = false;
    loadStatus();
  } catch (err) {
    showErr($("pair-err"), err);
  }
}

function renderPinResult(doc) {
  if (doc.action === "start") {
    return "<b>pin started</b><br>paths: " + (doc.paths?.length ? doc.paths.map(esc).join(", ") : "(whole folder)") +
      "<br>base peers recorded: " + esc(String(doc.base_peers_recorded ?? 0)) +
      "<br>started at: " + esc(doc.started_at ?? "?");
  }
  if (doc.action === "stop") {
    return "<b>pin stopped</b>" + (doc.was_pinned ? "" : " (was not pinned)") +
      "<br>held changes kept on disk: " + esc(String(doc.held_changes ?? 0)) +
      "<br>release later to reconcile them.";
  }
  return "<b>pin released</b><br>ops applied: " + esc(String(doc.ops_applied ?? 0)) +
    " · quarantined: " + esc(String(doc.quarantined ?? 0)) +
    " · conflicts recorded: " + esc(String(doc.conflicts_recorded ?? 0)) +
    "<br>total entries in conflict report now: " + esc(String(doc.conflicts_total ?? 0));
}

async function doPin(action) {
  hideAll($("pin-err"), $("pin-result"));
  const paths = action === "start" ? parsePaths($("pin-paths").value) : null;
  try {
    const doc = await api("/api/pin/" + action, { folder: null, paths });
    $("pin-result").innerHTML = renderPinResult(doc);
    $("pin-result").hidden = false;
    loadStatus();
    if (action === "release") loadConflicts();
  } catch (err) {
    showErr($("pin-err"), err);
  }
}

// ---- boot -----------------------------------------------------------------

$("share-btn").addEventListener("click", () => doShare(false));
$("share-anyway").addEventListener("click", () => doShare(true));
$("pair-form").addEventListener("submit", doPairAccept);
$("pin-form").addEventListener("submit", (ev) => { ev.preventDefault(); doPin("start"); });
$("pin-stop").addEventListener("click", () => doPin("stop"));
$("pin-release").addEventListener("click", () => doPin("release"));
$("conflicts-refresh").addEventListener("click", loadConflicts);

loadStatus().then((ok) => {
  if (ok) {
    startEvents();
  } else {
    // daemon warming up or down: poll gently until it answers
    const warm = setInterval(async () => {
      if (await loadStatus()) {
        clearInterval(warm);
        startEvents();
      }
    }, 2000);
  }
});
loadConflicts();
