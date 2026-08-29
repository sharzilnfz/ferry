"use strict";

/* ==========================================================================
   FERRY · MINIMAL FLUID GLASS ENGINE
   ========================================================================== */

const $ = (id) => document.getElementById(id);
const $$ = (sel) => document.querySelectorAll(sel);

let currentState = "synced";
let isHolding = false;
let soundEnabled = true;
let audioCtx = null;

/* Micro-Haptic Synthesizer */
function playHapticFeedback(type = "tick") {
  if (!soundEnabled) return;
  try {
    const AudioContextClass = window.AudioContext || window.webkitAudioContext;
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
    }
  } catch {
    // Graceful fallback
  }
}

/* Activity Feed */
function addActivity(title, timeStr = null) {
  const feed = $("activity-feed");
  if (!feed) return;

  const now = new Date();
  const time = timeStr || now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  const row = document.createElement("div");
  row.className = "flow-row";
  row.innerHTML = `
    <div class="row-left">
      <span class="row-title">${title}</span>
    </div>
    <span class="row-time">${time}</span>
  `;

  feed.prepend(row);

  while (feed.children.length > 5) {
    feed.removeChild(feed.lastChild);
  }
}

/* State Morphing */
function applyState(mode, triggerSound = true) {
  if (triggerSound) playHapticFeedback("snap");
  currentState = mode;

  const mainCard = $("main-card");
  const heroBeacon = $("hero-beacon");
  const beaconCore = heroBeacon.querySelector(".beacon-core");
  const beaconRing = heroBeacon.querySelector(".beacon-ring");
  const heroTitle = $("hero-title");
  const heroSub = $("hero-sub");
  const heroStateBadge = $("hero-state-badge");
  const metricHash = $("metric-hash");
  const metricHeld = $("metric-held");
  const metricConflicts = $("metric-conflicts");
  const btnPin = $("btn-pin");
  const btnPinLabel = $("btn-pin-label");
  const btnRelease = $("btn-release");
  const syncTrack = $("sync-track");
  const connBeacon = $("conn-beacon");
  const connText = $("conn-text");

  // Emil Kowalski Subtle Blur Morphing
  if (mainCard) {
    mainCard.classList.add("is-blur-transitioning");
    setTimeout(() => mainCard.classList.remove("is-blur-transitioning"), 180);
  }

  if (syncTrack) {
    syncTrack.classList.toggle("visible", mode === "syncing");
  }

  if (mode === "synced") {
    isHolding = false;
    beaconCore.style.backgroundColor = "var(--state-synced)";
    beaconCore.style.boxShadow = "0 0 14px var(--state-synced-glow)";
    beaconRing.style.borderColor = "var(--state-synced)";
    connBeacon.style.backgroundColor = "var(--state-synced)";
    connBeacon.style.boxShadow = "0 0 8px var(--state-synced-glow)";
    connText.textContent = "Active Session";

    heroTitle.textContent = "Synced";
    heroSub.textContent = "All folders match across peers. Continuous cryptographic verification active.";
    heroStateBadge.textContent = "Active";

    metricHash.textContent = "f4b9c100";
    metricHeld.textContent = "0";
    metricConflicts.textContent = "0";

    btnPinLabel.textContent = "Hold Edits";
    btnPin.style.display = "inline-flex";
    btnRelease.style.display = "none";

    addActivity("Continuous Sync Verified");
  } else if (mode === "syncing") {
    beaconCore.style.backgroundColor = "var(--state-syncing)";
    beaconCore.style.boxShadow = "0 0 14px var(--state-syncing-glow)";
    beaconRing.style.borderColor = "var(--state-syncing)";
    connBeacon.style.backgroundColor = "var(--state-syncing)";
    connBeacon.style.boxShadow = "0 0 8px var(--state-syncing-glow)";
    connText.textContent = "Syncing Delta";

    heroTitle.textContent = "Syncing...";
    heroSub.textContent = "Transferring 3 delta chunks over encrypted QUIC stream.";
    heroStateBadge.textContent = "3 Chunks";

    metricHash.textContent = "9a77e02b";

    addActivity("Delta Hydration Started");
  } else if (mode === "holding") {
    isHolding = true;
    beaconCore.style.backgroundColor = "var(--state-holding)";
    beaconCore.style.boxShadow = "0 0 14px var(--state-holding-glow)";
    beaconRing.style.borderColor = "var(--state-holding)";
    connBeacon.style.backgroundColor = "var(--state-holding)";
    connBeacon.style.boxShadow = "0 0 8px var(--state-holding-glow)";
    connText.textContent = "Holding Buffer";

    heroTitle.textContent = "Holding";
    heroSub.textContent = "Incoming remote edits safely buffered while local agent executes.";
    heroStateBadge.textContent = "2 Changes Held";

    metricHeld.textContent = "2";
    btnPinLabel.textContent = "Stop Hold";
    btnRelease.style.display = "inline-flex";

    addActivity("Work Protection Activated");
  } else if (mode === "conflict") {
    beaconCore.style.backgroundColor = "var(--state-conflict)";
    beaconCore.style.boxShadow = "0 0 14px var(--state-conflict-glow)";
    beaconRing.style.borderColor = "var(--state-conflict)";
    connBeacon.style.backgroundColor = "var(--state-conflict)";
    connBeacon.style.boxShadow = "0 0 8px var(--state-conflict-glow)";
    connText.textContent = "Conflict Alert";

    heroTitle.textContent = "1 Conflict";
    heroSub.textContent = "Conflicting edit safely quarantined. No local files overwritten.";
    heroStateBadge.textContent = "Quarantined";

    metricConflicts.textContent = "1";

    addActivity("Conflict File Quarantined");
  } else if (mode === "offline") {
    beaconCore.style.backgroundColor = "var(--state-offline)";
    beaconCore.style.boxShadow = "none";
    beaconRing.style.borderColor = "var(--state-offline)";
    connBeacon.style.backgroundColor = "var(--state-offline)";
    connBeacon.style.boxShadow = "none";
    connText.textContent = "Offline";

    heroTitle.textContent = "Offline";
    heroSub.textContent = "Ferry background daemon not running. Local store in idle mode.";
    heroStateBadge.textContent = "Disconnected";

    metricHash.textContent = "--------";
    metricHeld.textContent = "0";
    metricConflicts.textContent = "0";

    addActivity("Daemon Connection Lost");
  }

  $$(".pill").forEach((pill) => {
    pill.classList.toggle("active", pill.getAttribute("data-state") === mode);
  });
}

/* Theme Controller */
function initTheme() {
  const savedTheme = localStorage.getItem("ferry_fluid_theme") || "dark";
  document.documentElement.setAttribute("data-theme", savedTheme);
  updateThemeIcons(savedTheme);

  const savedSound = localStorage.getItem("ferry_fluid_sound");
  soundEnabled = savedSound !== "false";
  updateSoundIcons();
}

function updateThemeIcons(theme) {
  const moon = $("icon-theme-moon");
  const sun = $("icon-theme-sun");
  if (theme === "dark") {
    moon.style.display = "block";
    sun.style.display = "none";
  } else {
    moon.style.display = "none";
    sun.style.display = "block";
  }
}

function toggleTheme() {
  playHapticFeedback("tick");
  const cur = document.documentElement.getAttribute("data-theme") || "dark";
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", next);
  localStorage.setItem("ferry_fluid_theme", next);
  updateThemeIcons(next);
}

function updateSoundIcons() {
  const onIcon = $("icon-sound-on");
  const offIcon = $("icon-sound-off");
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
  localStorage.setItem("ferry_fluid_sound", soundEnabled ? "true" : "false");
  updateSoundIcons();
  if (soundEnabled) playHapticFeedback("success");
}

/* Modal Controller */
function openModal() {
  playHapticFeedback("tick");
  const modal = $("pair-modal");
  modal.classList.add("open");
  modal.setAttribute("aria-hidden", "false");
}

function closeModal() {
  playHapticFeedback("tick");
  const modal = $("pair-modal");
  modal.classList.remove("open");
  modal.setAttribute("aria-hidden", "true");
}

/* Event Setup */
function init() {
  initTheme();

  $("btn-theme").addEventListener("click", toggleTheme);
  $("btn-sound").addEventListener("click", toggleSound);

  $("btn-sync").addEventListener("click", () => {
    applyState("syncing");
    setTimeout(() => {
      applyState("synced");
      playHapticFeedback("success");
    }, 1000);
  });

  $("btn-pin").addEventListener("click", () => {
    if (isHolding) applyState("synced");
    else applyState("holding");
  });

  $("btn-release").addEventListener("click", () => {
    applyState("synced");
    playHapticFeedback("success");
    addActivity("Held Modifications Merged");
  });

  // Modal
  $("btn-pair").addEventListener("click", openModal);
  $("btn-close-modal").addEventListener("click", closeModal);
  $("pair-modal").addEventListener("click", (e) => {
    if (e.target === $("pair-modal")) closeModal();
  });

  $("btn-create-offer").addEventListener("click", () => {
    playHapticFeedback("snap");
    $("offer-box").style.display = "block";
    addActivity("Pair Token Created");
  });

  $("btn-copy-token").addEventListener("click", async () => {
    const tokenVal = $("token-display").value;
    try {
      await navigator.clipboard.writeText(tokenVal);
    } catch {
      // ignore
    }
    $("btn-copy-token").textContent = "Copied!";
    playHapticFeedback("success");
    setTimeout(() => {
      $("btn-copy-token").textContent = "Copy";
    }, 1500);
  });

  $("btn-accept").addEventListener("click", () => {
    const val = $("accept-input").value.trim();
    if (val) {
      playHapticFeedback("success");
      addActivity(`Paired: ${val}`);
      closeModal();
      $("accept-input").value = "";
    }
  });

  $("btn-clear").addEventListener("click", () => {
    playHapticFeedback("tick");
    $("activity-feed").innerHTML = "";
  });

  $$(".pill").forEach((pill) => {
    pill.addEventListener("click", () => {
      applyState(pill.getAttribute("data-state"));
    });
  });

  // Keyboard Navigation
  window.addEventListener("keydown", (e) => {
    if (["INPUT", "TEXTAREA"].includes(document.activeElement.tagName)) {
      if (e.key === "Escape") {
        document.activeElement.blur();
        closeModal();
      }
      return;
    }

    if (e.key === "1") applyState("synced");
    else if (e.key === "2") applyState("syncing");
    else if (e.key === "3") applyState("holding");
    else if (e.key === "4") applyState("conflict");
    else if (e.key === "5") applyState("offline");
    else if (e.key === "t" || e.key === "T") toggleTheme();
    else if (e.key === "p" || e.key === "P") {
      const modal = $("pair-modal");
      if (modal.classList.contains("open")) closeModal();
      else openModal();
    } else if (e.code === "Space") {
      e.preventDefault();
      $("btn-sync").click();
    } else if (e.key === "Escape") {
      closeModal();
    }
  });

  // Initial Seed
  addActivity("Continuous Sync Verified");
  addActivity("MacBook Pro Connected");
  addActivity("Session Authenticated");
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
