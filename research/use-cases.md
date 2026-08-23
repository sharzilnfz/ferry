# Use cases & pain points research

Research into how developers actually move whole project directories between machines today, where each method falls apart, and what agentic coding adds to the problem. Every claim links to a source. Compiled August 2026.

The market signal is real. Theo Browne (t3.gg) argued in May 2026 that "GitHub is dying and git is not the right primitive" and that worktrees are "an abomination," calling for source control that makes more assumptions about file systems ([Digg summary](https://digg.com/ai/060emup5)). His pattern at t3.gg is consistent: he builds tools only after publicly begging for them for years (Uploadthing came after three years of complaining that S3 was too hard) ([Mux interview](https://www.mux.com/blog/theobrowne)). A wave of small products now attacks exactly this space: [Tether CLI](https://github.com/paddo-tech/tether-cli), [Bowline](https://bowline.sh/use-cases/multiple-machines), [claude-sync](https://medium.com/codex/sync-your-claude-code-sessions-across-all-devices-2e407c2eb160). None has won.

---

## Archetype 1: Solo dev, desktop plus laptop

**Use case.** The most common shape. Work all day on a desktop, pick up the same project on a laptop in the evening, and vice versa. A Stack Exchange poster with a Windows PC and an Ubuntu laptop described switching "multiple times a day" and being unable to "just pick up where I left off on another" machine ([softwareengineering.stackexchange.com](https://softwareengineering.stackexchange.com/questions/398726/how-do-i-handle-2-or-more-parallel-development-environments-on-different-machine)).

**What they do today.**
- **Git.** Commit unfinished or broken work just to pull it elsewhere. The same poster rejected this because it means committing "even unfinished or completely broken files which seems like it would be a mess and quickly pollute my repo." Bowline's use-case page describes the identical ritual: "You commit or stash work-in-progress just to move it" ([bowline.sh](https://bowline.sh/use-cases/multiple-machines)).
- **A hand-built git server.** How-To Geek documents the full DIY recipe: SSH keys, a bare repo on one machine, manual push/pull habits ([howtogeek.com](https://www.howtogeek.com/i-turned-git-into-a-private-github-free-sync-system-between-my-own-machines-and-it-completely-changed-how-i-work/)). It works, but every transfer requires remembering to push.
- **Syncthing.** An Ask HN reply describes repos in `~/git` synced with Syncthing so work can be picked up on any machine "without interruption" ([news.ycombinator.com](https://news.ycombinator.com/item?id=41273716)). Setup cost is real: peer-to-peer means configuring every device, usually with an always-on server as the meeting point ([forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/)).

**What breaks.** Git covers only tracked, committed files. Untracked files, local notes, and `.env` "never make the trip, so the second machine is subtly not the first" ([bowline.sh](https://bowline.sh/use-cases/multiple-machines)). Forrest Jacobs put it plainly: "I don't want to track personal config files in version control, but I do want to sync them. And I don't always want to check in work in progress" ([forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/)). The "which box had the latest work?" problem is answered by guessing ([bowline.sh](https://bowline.sh/use-cases/multiple-machines)).

---

## Archetype 2: Cross-platform users (Windows desktop + Mac, Mac + Linux)

**Use case.** Developers who own a gaming PC and a MacBook, or a Mac plus a Linux box, and want one project tree that behaves identically everywhere.

**What they do today.** Forrest Jacobs codes on a MacBook and a Windows PC with wishes ranked in order: Dropbox-style magic sync, gitignore-style exclusion of host-specific dependencies, and coverage of a headless Linux server plus WSL. He tried OneDrive first (ignoring files is "awkward" and Windows-only, no Linux client), considered Dropbox (awkward ignoring, and he'd left over pricing), rejected remote development, and settled on Syncthing ([forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/)). Level1Techs forum members describe keeping environments aligned with Ansible playbooks, VS Code settings sync, and deliberately trimming their toolset so everything exists on both platforms ([forum.level1techs.com](https://forum.level1techs.com/t/how-do-you-all-like-to-keep-development-environments-in-sync-between-machines/184135)).

**What breaks.** Cloud drives mangle developer directories on macOS: Dropbox conflict-copy markers can break `npm install` mid-run when files get locked ([designdebt.club](https://designdebt.club/ignore-files-and-folders-from-dropbox-sync)), and one developer reports Dropbox conflict resolution broke an entire git repository, corrupting `.git` and losing data ([bigsweater.co](https://bigsweater.co/writing/stop-dropbox-syncing-node_modules-with-find-and-hammerspoon)). Symlinks are a known trap: community workarounds involve junction points, aliases, and watchers that set extended attributes on newly created folders, and they behave differently per OS ([community.dropbox.com](https://community.dropbox.com/en/discussion/388947/how-to-make-dropbox-ignore-node_modules-folder-with-symbolic-links-aliases)). File permission models differ across macOS, Linux, and NTFS enough that sync tools ship explicit default-mode flags to paper over the gap ([takken.io](https://takken.io/blog/seamless-windows-linux-development)).

---

## Archetype 3: WSL users

**Use case.** Windows developers running Linux toolchains in WSL2 who also want to open the same project from native Windows apps (browsers, IDEs, GitHub Desktop).

**What they do today.** Nothing good. The common advice is "store projects in the Linux file system" or "copy files rather than working directly across platform boundaries" ([github.com/webbertakken/wsl-filesystem-benchmark](https://github.com/webbertakken/wsl-filesystem-benchmark)). One developer runs Emacs against a Dropbox-synced NTFS folder, then rsyncs directories into WSL2's ext4 for long sessions and rsyncs back at the end of the day ([vxlabs.com](https://vxlabs.com/2019/12/06/wsl2-io-measurements/)). Another copies Rust projects into a temp dir inside WSL before building, cutting clean build time from 1m30s to 21s ([markentier.tech](https://markentier.tech/posts/2022/01/speedy-rust-builds-under-wsl2/)).

**What breaks.** The 9P protocol boundary is brutal. Benchmarks show WSL reading the Windows filesystem at roughly 6% of native speed (random reads as low as 3%), and Windows writing into WSL below 1% for large sequential writes ([takken.io](https://takken.io/blog/seamless-windows-linux-development), [github.com/webbertakken/wsl-filesystem-benchmark](https://github.com/webbertakken/wsl-filesystem-benchmark)). `git status`, `yarn install`, and dev servers run "10-20x longer when crossing filesystem boundaries" ([takken.io](https://takken.io/blog/seamless-windows-linux-development)). GitHub Desktop is essentially unusable on WSL repos: `git status` takes 2-6 seconds instead of 100-300ms ([github.com/desktop/desktop#22044](https://github.com/desktop/desktop/issues/22044)). A single stat-heavy Python script took 13.5s under `/mnt/c` on Windows 11 versus 0.05s natively ([github.com/microsoft/WSL#9430](https://github.com/microsoft/WSL/issues/9430)).

The sharpest insight here comes from Takken: the right fix is real-time bidirectional sync between the two filesystems, so each side works at native speed on its own copy, with Mutagen doing the bridging ([takken.io](https://takken.io/blog/seamless-windows-linux-development)). A sync tool that treats "Windows host filesystem" and "WSL guest filesystem" as just two peers would collapse this entire archetype's pain into nothing.

---

## Archetype 4: Laptop plus remote server or cloud VM

**Use case.** Heavy compute (CUDA, big builds) lives on a server or cloud box; the comfortable keyboard lives on a lap.

**What they do today.** SSH plus tmux ([level1techs forum](https://forum.level1techs.com/t/how-do-you-all-like-to-keep-development-environments-in-sync-between-machines/184135)); Mutagen sessions pushing local edits into remote containers ([datanovia.com](https://www.datanovia.com/blog/docker-file-sync-macos-mutagen)); Google built cdc-file-transfer at Stadia precisely because `scp` "always copies full files, there is no delta mode," is "slow for many small files," and made iterating on 40-45GB game builds impractical over home internet ([github.com/google/cdc-file-transfer](https://github.com/google/cdc-file-transfer)).

**What breaks.** Remote development dies with connectivity: "Blips in internet connectivity become big problems. At best, you wait for keystrokes to appear over SSH. At worst, you can't code at all," and weak VMs make compiling a complex Rust project frustrating ([forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/)). Mutagen helps but demands care: sessions go offline, conflicts must be resolved by hand, huge projects should be split into multiple sessions, and the Compose plugin is deprecated ([datanovia.com](https://www.datanovia.com/blog/docker-file-sync-macos-mutagen)). rsync handles none of this well bidirectionally; its own ecosystem recommends Unison or Syncthing instead, and Unison fails often across version mismatches ([resilio.com](https://www.resilio.com/blog/rsync-two-way), [stackoverflow.com](https://stackoverflow.com/questions/2936627/two-way-sync-with-rsync)).

---

## Archetype 5: Small teams sharing environments

**Use case.** Two to ten people who need identical project state including the parts git ignores: local env files, seed databases, caches.

**What they do today.** Version pinning (`nvmrc`, lockfiles), bootstrap scripts, devcontainers. JetBrains catalogs how each fails: someone forgets to source the `.env`, scripts aren't idempotent, Docker feels clunky, Nix is hard, and drift accumulates anyway. Their examples are exact: "Files like .env.local or ignored setup scripts go out of sync," tests pass for one dev and fail for another, onboarding becomes a scavenger hunt for misconfigured shells and missing env vars ([blog.jetbrains.com](https://blog.jetbrains.com/codecanvas/2025/08/configuration-drift-the-pitfall-of-local-machines/)). A dedicated article lists the hidden-state causes: undeclared tools, env vars present locally but not in CI, leftover caches and containers ([codeables.dev](https://codeables.dev/article/what-causes-works-on-my-machine-build-test-differences-between-dev)).

**What breaks.** Everything that lives outside git. The JetBrains piece names the root cause directly: config drift happens because ".env.local or ignored setup scripts go out of sync," and no existing approach enforces consistency without heavy friction ([blog.jetbrains.com](https://blog.jetbrains.com/codecanvas/2025/08/configuration-drift-the-pitfall-of-local-machines/)). Tether CLI exists specifically to sync `.env` files and IDE settings matched by git remote URL, with AES-256-GCM encryption and secret scanning before anything leaves the machine ([github.com/paddo-tech/tether-cli](https://github.com/paddo-tech/tether-cli)). Its existence proves the gap; its Git-backed design (sync latency measured in minutes, five-minute daemon polls) shows nobody has nailed real-time yet.

---

## Archetype 6: ML and game developers with huge binaries and datasets

**Use case.** Datasets, model checkpoints, textures, engine builds: gigabytes of unversioned or poorly-versioned binary data that must exist on every machine.

**What they do today.** Git LFS if they're small, Perforce if they're serious, rsync/scp scripts if they're desperate, cloud drives if they're brave.

**What breaks.** Git LFS "struggles with repositories exceeding 50GB," blocks files over 100MB in plain git, and does nothing for binary merges; artists still clobber each other's `.uasset` files because locking is opt-in discipline rather than enforced checkout ([perforce.com](https://www.perforce.com/blog/vcs/git-vs-perforce-how-choose-and-when-use-both), [teamcoherence.com](https://www.teamcoherence.com/git-lfs-game-development-game-assets)). Locking support is weak in GUIs, unlocking is manual, and LFS has significant disk overhead ([blog.rime.red](https://blog.rime.red/git-lfs-or-perforce-for-unreal-in-2024)). Project Borealis, a fully remote Unreal team, burned through GitLab.com instability and repo size limits, then self-hosted GitLab, then wrote custom Python sync tooling, NuGet binary distribution, and junction-link plumbing before things worked; support tickets dropped 95% only after all of that ([projectborealis.com](https://projectborealis.com/git-distributed-collaboration-at-scale/)). For ML-scale distribution, Voicebase needed Resilio's P2P to push 50GB+ language models to 400 servers in an hour instead of eight ([resilio.com](https://www.resilio.com/blog/rsync-two-way)). Plain rsync delta-match is slow at this scale too: Stadia's CDC-based tool hit 1500 MB/s of effective diffing versus rsync's 50 MB/s ([github.com/google/cdc-file-transfer](https://github.com/google/cdc-file-transfer)).

The opportunity note: these users don't need history for datasets and caches, they need *presence*. A sync layer that treats `data/`, `checkpoints/`, and `build/` as bulk-presence directories while keeping fine-grained history for source would serve them better than either git or Perforce licensing.

---

## Archetype 7: Agentic development, part one: agents run overnight on one machine

**Use case.** Kick off a six-hour refactor at 11pm, sleep, review the diff over coffee. Now mainstream practice with detailed guides ([phone-stack.com](https://phone-stack.com/blog/run-claude-code-overnight), [jeangalea.com](https://jeangalea.com/claude-code-overnight/)).

**What they do today.** Keep the machine awake (`caffeinate -i`, clamshell mode, Windows `powercfg` lid action), run headless with budget caps, review logs in the morning ([jeangalea.com](https://jeangalea.com/claude-code-overnight/), [pasqualepillitteri.it](https://pasqualepillitteri.it/en/news/779/disable-laptop-sleep-lid-close-ai-agents)). Tools like Night Shift and Nightcrawler wrap Claude Code in supervised loops with checkpoints and handoff files ([github.com/ppuliu/night-shift](https://github.com/ppuliu/night-shift), [github.com/thebasedcapital/nightcrawler](https://github.com/thebasedcapital/nightcrawler)). Others moved the whole agent to an always-on workstation because "your laptop sleeps... When the laptop sleeps, the process is suspended" and network transitions compound failures ([phone-stack.com](https://phone-stack.com/blog/run-claude-code-overnight)).

**What breaks.** Once the agent runs on machine A (desktop, home server, cloud VM) and the human opens the repo on machine B (laptop), the work product lives on A. Today the bridge is git push/pull, which reintroduces the commit-to-move dance for what is fundamentally a file-state problem. Worse, concurrent access is unsafe: if the human opens the repo on B while the agent writes on A, generic sync tools produce conflict copies mid-write or silently last-writer-wins. Mutagen holds both versions and makes you resolve by hand ([datanovia.com](https://www.datanovia.com/blog/docker-file-sync-macos-mutagen)); Dropbox creates "conflicting copy" duplicates that break builds ([bigsweater.co](https://bigsweater.co/writing/stop-dropbox-syncing-node_modules-with-find-and-hammerspoon)). Night Shift itself implements "drift check before every write; external HEAD change stops the run" as a guardrail, which is exactly the concurrency problem surfacing inside agent tooling ([github.com/ppuliu/night-shift](https://github.com/ppuliu/night-shift)).

This archetype needs something no current tool provides: awareness that a directory has an active writer, so machine B can present a read-only or branch-like view while the agent works, then converge safely.

---

## Archetype 8: Agentic development, part two: agent state and memory must travel

**Use case.** Claude Code stores skills, commands, plugins, settings, API keys, and per-project memory in `~/.claude/`. OpenCode, Codex, and similar tools have equivalents. Users with two machines lose accumulated context daily.

**Evidence of demand, which is unusually loud:**
- A Claude Code feature request titled "Portable project memory across machines" documents that memory paths derive from absolute project paths, so different usernames or mount points silently orphan memory, plans, and session data on each machine; users syncing folders via iCloud/Syncthing/Dropbox "lose all accumulated project context each time" ([github.com/anthropics/claude-code#25739](https://github.com/anthropics/claude-code/issues/25739)).
- Rockford Lhotka works across three PCs plus Windows/WSL splits, "as many as six different environments where I run Claude Code against the same set of repositories," and wrote his own merge-aware sync for `~/.claude/projects/*/memory/` ([blog.lhotka.net](https://blog.lhotka.net/2026/05/08/Claude-Memory-Sync)).
- Nick Ang whitelists `CLAUDE.md`, settings, memory dirs, skills, and plugin config into a git dotfiles repo with a LaunchAgent for two-way sync, explicitly excluding session logs, telemetry, and caches as "machine-specific junk" ([nickang.com](https://nickang.com/how-to-sync-claude-code-global-files-across-machines-if-you-work-on-multiple-computers)).
- steeman.be routes `~/.claude` through a NAS plus symlinks plus Syncthing after "one too many times of building a great custom skill on one machine and having to manually copy it to the other" ([steeman.be](https://www.steeman.be/posts/syncing-claude-code-across-multiple-machines)).
- claude-sync encrypts the entire `~/.claude` tree and pushes it to S3/R2/GCS ([medium.com/codex](https://medium.com/codex/sync-your-claude-code-sessions-across-all-devices-2e407c2eb160)).
- A YouTube workflow syncs CLAUDE.md via Dropbox for non-technical teams, noting username-path mismatches break it ([youtube.com](https://www.youtube.com/watch?v=qIWpa582Pso)).

Five independent hand-rolled solutions for one directory is a strong signal. What breaks in each: git-based approaches need commits and break on symlinks; NAS-symlink approaches need LAN presence; S3 approaches add encryption key management; all of them must carefully separate sync-worthy state (memory, skills, CLAUDE.md) from churning junk (sessions, caches, history). A general-purpose project sync that understands dot-directory hygiene would absorb all of this.

---

## Archetype 9: Dotfiles and secrets (.env) sync

**Use case.** Same shell, same editor config, same API keys on every machine, without leaking credentials.

**What they do today.** Chezmoi, mackup, plain git repos with `.gitignore` gymnastics, encrypted splits via `pass` or git-secret ([linux.codidact.com](https://linux.codidact.com/posts/290202/290452?sort=age)), or newer all-in-one tools like Tether ([github.com/paddo-tech/tether-cli](https://github.com/paddo-tech/tether-cli)).

**What breaks.** The stakes are measurable. GitGuardian counted 23.8 million new secrets in public GitHub commits in 2024, up 25% year over year, with 35% of private repos containing plaintext secrets ([gitguardian.com](https://www.gitguardian.com/state-of-secrets-sprawl-report-2025), [blog.gitguardian.com](https://blog.gitguardian.com/the-state-of-secrets-sprawl-2025/)). In 2025 it hit 28.65M, and roughly 65% of leaked secrets appeared in environment-variable configuration files; attackers actively mass-scan for exposed `.env` files ([darkreading.com](https://www.darkreading.com/threat-intelligence/attackers-targeting-developer-secrets)). A `.env` file carries a 54% chance of containing a detectable secret ([GitGuardian 2024 report](https://assets.zyrosite.com/dOqbJe6jweHoPWVn/the-state-of-secrets-sprawl-report-2024-by-gitguardian-AwvM98Ve3MsPvgxo.pdf)). Meanwhile AI-assisted commits leak at roughly twice baseline rate, and MCP configuration files exposed 24,008 unique secrets ([blog.gitguardian.com 2026](https://blog.gitguardian.com/the-state-of-secrets-sprawl-2026/)). The takeaway for a sync tool: it will carry `.env` files whether it wants to or not, because that's what users ask for, so end-to-end encryption and leak detection are table stakes, not features ([tether-cli.com](https://tether-cli.com/)).

---

## Archetype 10: Ephemeral and cloud dev machines

**Use case.** Codespaces, Gitpod/Ona, rented GPU boxes, disposable agent sandboxes. Hydrate a full project, including deps and caches, fast; dissolve it without losing state.

**What they do today.** Stripe gives engineers EC2 devboxes provisioned from a standard image, synced "with rsync from their laptop"; Ona argues fleets of coding agents "don't fit on a laptop" and each needs fully provisioned environments ([ona.com](https://ona.com/stories/the-last-year-of-localhost)). Local parallel agents via git worktrees fail visibly: "Each worktree needs its own dependency install... You end up with port conflicts, shared caches corrupting each other, and a machine that grinds to a halt," and monorepo setup from scratch can take days ([ona.com](https://ona.com/stories/the-last-year-of-localhost)).

**What breaks.** Hydration is the bottleneck. Cloning gets you tracked files only; `node_modules`, model weights, and local databases need separate rebuild steps. The CDE pitch itself admits local setups persisted because "zero latency, years of customization, a workflow that felt like identity" beat streamed environments, and AI agents are the first workload heavy enough to force migration ([ona.com](https://ona.com/stories/the-last-year-of-localhost)). A fast, secure, resumable directory sync between laptop and ephemeral machine would let developers keep local-first workflows while renting compute by the hour, which is precisely the hybrid neither Codespaces nor Syncthing serves well today.

---

## Cross-cutting findings

Three facts cut across every archetype:

1. **Cloud drives structurally fail on code.** Thousands of small files defeat their indexers (two days to index `node_modules` ([peter.grman.at](https://peter.grman.at/ignore-node_modules-in-dropbox/))), their conflict handling corrupts repos ([bigsweater.co](https://bigsweater.co/writing/stop-dropbox-syncing-node_modules-with-find-and-hammerspoon)), their ignore mechanisms are fragile extended attributes ([designdebt.club](https://designdebt.club/ignore-files-and-folders-from-dropbox-sync)), and selective sync semantics have silently changed under users ([reddit.com/r/dropbox](https://www.reddit.com/r/dropbox/comments/dewl2a/cant_use_selective_sync_to_ignore_node_modules)).

2. **Dev-tool sync exists but is expert-only.** Mutagen, Syncthing, and Unison all solve pieces; all require manual configuration, ignore-list maintenance (one developer maintains a 60-line ignore YAML for WSL ([takken.io](https://takken.io/blog/seamless-windows-linux-development))), server infrastructure for reliability ([forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/)), and hand conflict resolution ([datanovia.com](https://www.datanovia.com/blog/docker-file-sync-macos-mutagen)).

3. **Agentic workflows multiply the stakes.** Agents write continuously for hours ([jeangalea.com](https://jeangalea.com/claude-code-overnight/)), accumulate irreplaceable context in dot-directories ([github.com/anthropics/claude-code#25739](https://github.com/anthropics/claude-code/issues/25739)), and increasingly run on machines the human isn't sitting at ([phone-stack.com](https://phone-stack.com/blog/run-claude-code-overnight)). Sync tools designed for human-paced editing don't handle machine-paced writes.

---

## Top 10 sharpest pain points, ranked

1. **Git cannot carry working state.** Uncommitted edits, untracked files, `.env`, local notes, and agent memory never travel between machines, forcing the commit-broken-work ritual or silent divergence ([bowline.sh](https://bowline.sh/use-cases/multiple-machines), [forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/), [stackoverflow.com](https://stackoverflow.com/questions/41867151/preferred-method-of-syncing-untracked-changes-across-multiple-machines)).

2. **Cloud drives destroy developer directories.** Days-long `node_modules` indexing ([peter.grman.at](https://peter.grman.at/ignore-node_modules-in-dropbox/)), npm installs corrupted by locked files ([designdebt.club](https://designdebt.club/ignore-files-and-folders-from-dropbox-sync)), whole git repos lost to conflict-copy logic ([bigsweater.co](https://bigsweater.co/writing/stop-dropbox-syncing-node_modules-with-find-and-hammerspoon)).

3. **Agent session/memory state is trapped per machine.** Path-derived memory directories orphan context on every new machine or username; five separate community tools exist purely to sync `~/.claude` ([github.com/anthropics/claude-code#25739](https://github.com/anthropics/claude-code/issues/25739), [blog.lhotka.net](https://blog.lhotka.net/2026/05/08/Claude-Memory-Sync), [nickang.com](https://nickang.com/how-to-sync-claude-code-global-files-across-machines-if-you-work-on-multiple-computers), [steeman.be](https://www.steeman.be/posts/syncing-claude-code-across-multiple-machines), [medium.com/codex](https://medium.com/codex/sync-your-claude-code-sessions-across-all-devices-2e407c2eb160)).

4. **WSL cross-filesystem performance forces manual copy dances.** 3-14% of native speed across the 9P boundary, 10-20x slower git operations, and hand-rolled rsync round trips as the workaround ([takken.io](https://takken.io/blog/seamless-windows-linux-development), [github.com/desktop/desktop#22044](https://github.com/desktop/desktop/issues/22044), [vxlabs.com](https://vxlabs.com/2019/12/06/wsl2-io-measurements/)).

5. **No safe story for agent-writes-while-human-watches.** An agent grinding on machine A while the repo is opened on machine B hits conflict-copy chaos (cloud drives) or manual merge queues (Mutagen); agent frameworks already implement their own stop-the-world guards because no sync tool does it ([github.com/ppuliu/night-shift](https://github.com/ppuliu/night-shift), [datanovia.com](https://www.datanovia.com/blog/docker-file-sync-macos-mutagen), [bigsweater.co](https://bigsweater.co/writing/stop-dropbox-syncing-node_modules-with-find-and-hammerspoon)).

6. **Secrets sprawl makes `.env` sync dangerous by default.** 65% of leaked secrets live in env files, attackers scan for them constantly, and private repos leak at 8x public rates; any sync tool touching `.env` needs E2E encryption and detection ([darkreading.com](https://www.darkreading.com/threat-intelligence/attackers-targeting-developer-secrets), [blog.gitguardian.com 2025](https://blog.gitguardian.com/the-state-of-secrets-sprawl-2025/)).

7. **Large-binary hydration has no middle path.** Between Perforce licenses and LFS's ~50GB ceiling sit indie teams and ML folks shoveling 40GB builds with scp or rsync at 50 MB/s effective diffing ([teamcoherence.com](https://www.teamcoherence.com/git-lfs-game-development-game-assets), [github.com/google/cdc-file-transfer](https://github.com/google/cdc-file-transfer), [blog.rime.red](https://blog.rime.red/git-lfs-or-perforce-for-unreal-in-2024)).

8. **Two-way sync tools are expert-only.** rsync can't do bidirectional safely (deletion propagation breaks), Unison fails across version mismatches, Syncthing needs an always-on server and per-device config, Mutagen sessions wedge and need splitting ([unix.stackexchange.com](https://unix.stackexchange.com/questions/12197/syncing-directories-in-both-directions-with-rsync), [stackoverflow.com](https://stackoverflow.com/questions/2936627/two-way-sync-with-rsync), [forrestjacobs.com](https://forrestjacobs.com/using-syncthing-to-sync-coding-projects/), [datanovia.com](https://www.datanovia.com/blog/docker-file-sync-macos-mutagen)).

9. **Environment drift starts outside git.** `.env.local`, ignored setup scripts, and global tool versions diverge until "works on my machine" consumes team hours; onboarding takes days ([blog.jetbrains.com](https://blog.jetbrains.com/codecanvas/2025/08/configuration-drift-the-pitfall-of-local-machines/), [codeables.dev](https://codeables.dev/article/what-causes-works-on-my-machine-build-test-differences-between-dev)).

10. **Overnight agents die with the laptop, and their output strands on the runtime machine.** Sleep kills long runs, pushing users to dedicated workstations whose file state then needs a second-class git-shaped bridge back to the laptop ([phone-stack.com](https://phone-stack.com/blog/run-claude-code-overnight), [lidrun.com](https://lidrun.com/blog/keep-claude-code-running-when-macbook-closed), [ona.com](https://ona.com/stories/the-last-year-of-localhost)).
