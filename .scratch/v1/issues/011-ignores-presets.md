# T-011: Ignore rules and dev presets

Status: ready-for-agent
Depends on: T-004

Gitignore-syntax ignore files (`ferry.ignore`, plus honoring `.gitignore`
opt-in), tuned defaults per SPEC: caches out, `node_modules` opt-in,
`.env` opt-in with a loud warning. Agent-state presets: `ferry preset claude`
configures `~/.claude` selective rules (memory/skills/CLAUDE.md in, session
logs/caches out) per the research findings on agent state portability.

Acceptance: preset round-trips the documented include/exclude set; secret
scan heuristic flags likely-credential `.env` inclusion with a warning at
share time.
