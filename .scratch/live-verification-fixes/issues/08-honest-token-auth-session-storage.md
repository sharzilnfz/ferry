# 08: Honest Token Authentication & Session Storage Flow

Status: ready-for-agent
Depends on: 07-minimalist-web-ui-overhaul.md
Blocks: 09-e2e-live-process-and-browser-verification.md

**What to build:**
Reconcile and streamline the security model of the web interface. When a user opens the dashboard via an authorized URL containing `?token=<hex>`, the client extracts and stores the token in `sessionStorage`, attaching it automatically as a bearer token on all subsequent API requests across page reloads. Remove contradictory claims of "no auth by design" from the footer, replace raw 403 error screens with a clean token input prompt for unauthenticated visits, and update the footer to honestly reflect the security model (`Localhost only · Protected by session token`).

**Blocked by:** 07: Minimalist, Zero-Jargon Web Interface Overhaul (Captive Portal Style)

### Acceptance Criteria

- [ ] Navigating to the dashboard with `?token=<hex>` caches the token in `sessionStorage` and attaches `Authorization: Bearer <token>` to all subsequent API queries.
- [ ] Refreshing the dashboard or opening new tabs within the same browser session retains authentication without requiring the token parameter in the URL.
- [ ] Accessing the dashboard without an active token presents a minimal, friendly token entry dialog instead of a broken error view.
- [ ] The dashboard footer accurately describes the security model (`Localhost only · Protected by session token`), eliminating contradictory messaging.
- [ ] Automated HTTP tests verify that valid tokens are accepted, missing tokens trigger a friendly prompt, and invalid tokens are rejected cleanly.
