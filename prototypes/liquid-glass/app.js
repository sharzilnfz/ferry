"use strict";

const $ = (id) => document.getElementById(id);
const $$ = (sel) => document.querySelectorAll(sel);

let isHolding = false;

// Activity Icons (Apple Minimal SVG hairline)
const ICONS = {
  check: '<svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg>',
  sync: '<svg viewBox="0 0 24 24"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>',
  lock: '<svg viewBox="0 0 24 24"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>',
  alert: '<svg viewBox="0 0 24 24"><path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>',
  device: '<svg viewBox="0 0 24 24"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>'
};

function addActivity(title, detail, iconType = "check") {
  const feed = $("activity-feed");
  if (!feed) return;

  const now = new Date();
  const timeStr = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  const item = document.createElement("div");
  item.className = "activity-item";
  item.innerHTML = `
    <div class="activity-left">
      <div class="activity-icon-badge">
        ${ICONS[iconType] || ICONS.check}
      </div>
      <div class="activity-title-group">
        <span class="activity-title">${title}</span>
        <span class="activity-detail">${detail}</span>
      </div>
    </div>
    <span class="activity-time">${timeStr}</span>
  `;

  feed.prepend(item);

  while (feed.children.length > 8) {
    feed.removeChild(feed.lastChild);
  }
}

function applyState(mode) {
  const dot = $("hero-dot");
  const title = $("hero-title");
  const sub = $("hero-sub");
  const peers = $("metric-peers");
  const held = $("metric-held");
  const conflicts = $("metric-conflicts");
  const pinBadge = $("pin-status-badge");
  const btnPin = $("btn-pin");
  const btnRelease = $("btn-release");

  dot.className = "status-dot";

  if (mode === "synced") {
    isHolding = false;
    title.textContent = "Synced";
    sub.textContent = "All folders match across your devices.";
    if (peers) peers.textContent = "2 Devices";
    if (held) held.textContent = "0";
    if (conflicts) conflicts.textContent = "0";
    if (pinBadge) pinBadge.textContent = "Inactive";
    if (btnPin) btnPin.textContent = "Start Hold";
    if (btnRelease) btnRelease.style.display = "none";
    addActivity("Continuous Sync Verified", "Local tree matches peer cluster", "check");
  } else if (mode === "syncing") {
    dot.classList.add("dot-syncing");
    title.textContent = "Syncing...";
    sub.textContent = "Transferring 3 updated files over QUIC channel.";
    if (peers) peers.textContent = "1 Syncing";
    if (held) held.textContent = "0";
    addActivity("Delta Hydration Started", "Transferring 3 updated chunks to MacBook Pro", "sync");
  } else if (mode === "holding") {
    isHolding = true;
    dot.classList.add("dot-holding");
    title.textContent = "Holding";
    sub.textContent = "Incoming remote edits held while you work.";
    if (held) held.textContent = "2 edits";
    if (pinBadge) pinBadge.textContent = "Holding (2 Changes)";
    if (btnPin) btnPin.textContent = "Stop Hold";
    if (btnRelease) btnRelease.style.display = "inline-flex";
    addActivity("Work Protection Active", "2 incoming peer edits buffered safely", "lock");
  } else if (mode === "conflict") {
    dot.classList.add("dot-conflict");
    title.textContent = "1 Conflict";
    sub.textContent = "Conflicting file quarantined safely.";
    if (conflicts) conflicts.textContent = "1 Quarantined";
    addActivity("Quarantine Alert", "docs/architecture.md.ferry-conflict preserved", "alert");
  } else if (mode === "offline") {
    dot.classList.add("dot-offline");
    title.textContent = "Offline";
    sub.textContent = "Ferry background daemon not running.";
    if (peers) peers.textContent = "0 Devices";
    if (held) held.textContent = "0";
    addActivity("Daemon Offline", "Local process disconnected", "alert");
  }

  $$(".sim-pill").forEach((pill) => {
    pill.classList.toggle("active", pill.getAttribute("data-state") === mode);
  });
}

function initTheme() {
  const saved = localStorage.getItem("ferry_bold_theme") || "dark";
  document.documentElement.setAttribute("data-theme", saved);
}

function toggleTheme() {
  const cur = document.documentElement.getAttribute("data-theme") || "dark";
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", next);
  localStorage.setItem("ferry_bold_theme", next);
}

function init() {
  initTheme();
  $("btn-theme").addEventListener("click", toggleTheme);

  $("btn-sync").addEventListener("click", () => {
    applyState("syncing");
    setTimeout(() => applyState("synced"), 1000);
  });

  $("btn-pin").addEventListener("click", () => {
    if (isHolding) applyState("synced");
    else applyState("holding");
  });

  $("btn-release").addEventListener("click", () => {
    applyState("synced");
    addActivity("Work Protection Released", "Held modifications merged cleanly", "check");
  });

  // Modal
  $("btn-pair").addEventListener("click", () => $("pair-modal").classList.add("open"));
  $("btn-close-modal").addEventListener("click", () => $("pair-modal").classList.remove("open"));
  $("pair-modal").addEventListener("click", (e) => {
    if (e.target === $("pair-modal")) $("pair-modal").classList.remove("open");
  });

  $("btn-create-offer").addEventListener("click", () => {
    $("offer-box").style.display = "block";
    addActivity("Pair Token Created", "FERRY-PAIR-8849-01BC ready to share", "device");
  });

  $("btn-accept").addEventListener("click", () => {
    const val = $("accept-input").value.trim();
    if (val) {
      addActivity("Pair Offer Accepted", val, "device");
      $("pair-modal").classList.remove("open");
    }
  });

  $("btn-clear").addEventListener("click", () => {
    $("activity-feed").innerHTML = "";
  });

  $$(".sim-pill").forEach((pill) => {
    pill.addEventListener("click", () => applyState(pill.getAttribute("data-state")));
  });

  // Initial Seed Activity Feed Items
  addActivity("Continuous Sync Verified", "Local tree matches peer cluster", "check");
  addActivity("MacBook Pro M3 Connected", "Encrypted QUIC link · 4ms latency", "device");
  addActivity("Ferry Daemon Attached", "Localhost session token authenticated", "check");
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
