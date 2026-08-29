# Ferry UI Prototype: Fluid Glass (Apple Design & Emil Design Eng)

An elevated, fluid glass desktop synchronization dashboard for Ferry, built by strictly translating **Apple Design Principles (WWDC Fluid Interfaces & VisionOS)** and **Emil Kowalski's Design Engineering Philosophy** into web architecture.

This prototype maintains total font and structural consistency with the Ferry design system while delivering deep physical fluidity, true translucent materials, and tactile micro-interactions.

---

## Key Design & Engineering Features

### 1. Liquid Materials & Translucent Depth (VisionOS & macOS Sequoia)
- **Multi-layered Frosted Glass**: True `backdrop-filter: blur(28px) saturate(190%)` with ambient background gradient diffusion.
- **Specular Hairline Edge Lighting**: `1px solid rgba(255, 255, 255, 0.16)` top highlight catching light, contrasting with dark rim underneath.
- **Inner Light Volume**: Refractive radial gradients giving cards visual thickness without heavy drop shadows.
- **Precision Phosphor Beacons**: Dynamic status colors with layered glow and soft breathing pulse animation:
  - **Emerald** (`#30d158`) — *Synced & Verified*
  - **Ice Cyan** (`#0a84ff`) — *Active Delta Syncing*
  - **Topaz Amber** (`#ff9f0a`) — *Work Protection / Holding*
  - **Ruby** (`#ff453a`) — *Quarantined Conflict Alert*
  - **Slate** (`#8e8e93`) — *Offline Daemon*

### 2. Optical Typography & Font Consistency
- **Preserved Font Stack**:
  - System Display: `-apple-system, BlinkMacSystemFont, "SF Pro Display", "SF Pro Text", "Segoe UI", "Helvetica Neue", sans-serif`
  - Monospace Telemetry: `"SF Mono", "JetBrains Mono", Menlo, Monaco, monospace`
- **Size-Specific Tracking & Leading**:
  - Display Headings (`24px`): `letter-spacing: -0.038em`, `line-height: 1.08`
  - Card Titles (`13.5px`): `letter-spacing: -0.022em`, `font-weight: 700`
  - Micro Tags & Pills: `letter-spacing: +0.04em`, `text-transform: uppercase`, `font-size: 9.5px–10px`
  - Monospace Data: Clean alignment with tabulated numerals for hashes and latencies.

### 3. Motion & Micro-Interactions (Emil Kowalski Principles)
- **Response On Pointer-Down**: Instant tactile feedback on press with `transform: scale(0.97)` and fast 140ms release curves.
- **Never Scale from 0**: Element entrances scale from `0.96` with opacity crossfades.
- **Emil Kowalski Blur Morphing**: Instant 150ms state morphing with `filter: blur(2.5px)` crossfade to eliminate jarring layout swaps.
- **Origin-Aware Modals**: Pair modal transforms directly outward from the invoking button location.
- **Micro-Haptic Audio Synthesizer**: Web Audio API micro-ticks on button presses and state transitions for immediate multimodal confirmation (toggleable in header).
- **Staggered Activity Cascade**: Telemetry items animate smoothly with 35ms staggered spring timing.
- **Accessibility Ready**: Full support for `prefers-reduced-motion` and `prefers-reduced-transparency`.

---

## Interactive Features

- **Floating Glass Simulator Dock**: Instant one-click switching between system states (`Synced`, `Syncing`, `Holding`, `Conflicts`, `Offline`).
- **Interactive Work Protection**: Buffer incoming remote edits with path filters, track active state, and trigger clean merge releases.
- **Device Pairing Modal**: One-click token generation, token copy with instant confirmation, and offer path acceptance.
- **Telemetry Stream**: Live activity logging with filtering, clear actions, and real-time timestamps.
- **Dark / Light Glass Themes**: Frosted Obsidian Glass vs. Crystal Frost Glass with local persistence.

---

## Keyboard Shortcuts

| Key | Action |
| --- | --- |
| `1` | Set state to **Synced** |
| `2` | Set state to **Syncing** |
| `3` | Set state to **Holding** |
| `4` | Set state to **Conflicts** |
| `5` | Set state to **Offline** |
| `Space` | Trigger **Sync Now** |
| `T` | Toggle **Dark / Light Theme** |
| `P` | Open / Close **Pair Device Modal** |
| `Escape` | Close Modal / Clear Input Focus |

---

## Running the Prototype

Open `index.html` directly in any modern browser:

```bash
open prototypes/fluid-glass/index.html
```

Or run via any local static server:

```bash
# Python
python3 -m http.server 8000 --directory prototypes/fluid-glass

# Node / pnpm
npx serve prototypes/fluid-glass
```
