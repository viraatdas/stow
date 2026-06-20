# Stow — Documentation

Stow offloads files from your Mac to **your own** AWS S3 bucket to free disk
space, and brings them back on demand. It works two complementary ways:

1. **Transparent cloud folder** (Dropbox Smart Sync style) — a **Stow** folder in
   Finder. Files you put there live in S3; opening one downloads it back
   automatically, with no command.
2. **Whole-account auto-offload** (CLI) — a daily background job that offloads
   large, unused files *in place* anywhere under your home directory. These are
   transparent too: opening an offloaded file auto-downloads it (v0.5+).

Plus: **permanent share links** for any file or folder (`stow share`), and weekly
cleanup of regenerable tool caches (`stow clean`).

Everything stays in your AWS account. Stow never sees your data. Apple Silicon,
macOS 14+.

---

## Architecture

Four components share one App Group container (`group.ai.exla.stow`), which holds
the config + SQLite indexes that all of them read/write:

| Component | What it is | Role |
|---|---|---|
| `stow` | CLI (`/opt/homebrew/bin/stow`) | user-facing: `init`, `offload`, `restore`, `status`, `scan`, `auto`, `schedule`, `config` |
| `StowAgent.app` | faceless agent (`LSUIElement`, no Dock icon) | registers the File Provider domain, runs the **hourly auto-evict sweep** for the Stow folder; always-on via LaunchAgent |
| `StowFileProvider.appex` | `NSFileProviderReplicatedExtension` | the transparent folder: `createItem` (upload), `fetchContents` (download-on-open), `enumerator` |
| `libstow_core` | Rust static lib → `StowCore.xcframework` | S3 I/O (`aws-sdk-s3`), SQLite, blake3 hashing/dedup; linked into all three via a C FFI |

```
stow/
├─ cli/stow/                  # the `stow` CLI (dependency-free Swift)
├─ apps/StowAgent/            # faceless agent (domain registration, evictor)
│  └─ Resources/AppIcon.icns  # diving-stick-figure icon
├─ apps/StowFileProvider/     # File Provider extension
├─ packages/StowCore/         # Swift wrapper over the Rust C FFI
├─ rust/stow_core/            # Rust core: engine, provider, s3, index, config, ffi
├─ packaging/homebrew/        # Homebrew formula (build-from-source)
├─ packaging/launchd/         # LaunchAgent plist
├─ scripts/                   # build-rust-xcframework.sh, sign-notarize.sh
└─ site/                      # stow.viraat.dev landing page
```

Content is **content-addressed** in S3 (`fp/<blake3>` for the folder,
`objects/<blake3>` for the CLI), so identical files de-duplicate automatically,
and every restore is verified byte-for-byte against its hash.

---

## Mode 1 — The transparent Stow folder (Dropbox-style)

A **Stow** entry appears in Finder's sidebar at
`~/Library/CloudStorage/StowAgent-Stow`.

- **Drop a file in** → `createItem` uploads it to S3.
- It can go **dataless** (0 bytes on disk) but still shows in Finder.
- **Open it in any app** → `fetchContents` downloads it from S3 transparently,
  byte-identical. **No `stow restore` needed.**

### Auto-eviction of the Stow folder (automatic offload)

The agent runs an **hourly sweep** (`Evictor`) that offloads files in the Stow
folder which are **≥10 MB** and **untouched ≥30 days** (the same defaults as the
CLI; reads `policy` from the shared config). Eviction calls
`NSFileProviderManager.evictItem`, which drops the local copy to dataless while
the bytes stay in S3 — reading the file later re-downloads it transparently.

"Untouched" is tracked by a `last_access` timestamp in the provider DB: set on
`createItem`, **bumped on every `fetchContents`**, so a file you actually use
keeps resetting its clock and is never offloaded. (The agent is sandboxed and
can't `stat` the user-visible `~/Library/CloudStorage` path, so staleness lives
in the DB rather than reading the file on disk.)

Touch the sentinel `…/Group Containers/3C4383262W.ai.exla.stow/sweep-now` to force
an immediate sweep instead of waiting for the hourly timer.

**`stow status`** lists offloaded files from *both* modes: a **Stow folder**
section (folder files currently dataless — open one to download it back) and an
**in-place** section (CLI offloads — `stow restore` to bring back). The DB's
`dataless` flag is kept in sync by the agent (set after a successful `evictItem`,
cleared on `fetchContents`), so the list stays accurate both ways.

### One-time setup (required, by Apple's design)

macOS will not let an app enable its own File Provider extension (security: it
would let any app hijack Finder). So after install you must flip one toggle —
exactly like Dropbox's first run:

**System Settings → General → Login Items & Extensions → Extensions → File
Provider (ⓘ) → turn Stow ON → Done.**

Until this is on, the domain is `user-disabled` and every file op fails with
`-2011 NSFileProviderErrorDomainDisabled`. Verify with:

```sh
fileproviderctl diagnose -o /tmp/fpd && grep -i 'user-disabled\|enabled:' \
  /tmp/fpd/FileProvider/ai.exla.stow.fileprovider/*dump.log
```

You want to see `enabled: yes` (not `(⏹ user-disabled)`).

---

## Mode 2 — Whole-account auto-offload (CLI, in place)

The File Provider folder can't host files *outside* it (an OS limitation — same
for Dropbox/iCloud). Stow bridges that: offloading a file anywhere on disk
uploads it to S3, creates a **dataless mirror** in a hidden `.stow-inplace` area
of the Stow folder (pointing at the same S3 object — no second upload), and
replaces the original with a **symlink to the mirror**. Opening the original
path resolves into the dataless file, macOS calls our `fetchContents`, and the
app gets the real bytes — the download is invisible. The agent's hourly evictor
later re-offloads hydrated mirrors once they go stale again, so the space comes
back automatically.

```sh
stow init                 # detect AWS creds, auto-create the S3 bucket, save config
stow scan                 # dry run: what the policy would offload (no changes)
stow auto                 # offload everything matching the policy now
stow schedule [HH:MM]     # run `auto` automatically every day (default 12:00)
stow unschedule           # stop the daily run
stow offload <path>       # offload one specific file (opens auto-download)
stow restore <path>       # pin an offloaded file back on disk (byte-identical)
stow migrate              # upgrade pre-0.5 stub offloads to auto-download
stow status               # what's offloaded, space saved
```

**Default policy** (conservative — tune with `stow config`):

| Setting | Default | Change with |
|---|---|---|
| min size | 10 MB | `stow config set-size <MB>` |
| min age (untouched) | 30 days | `stow config set-age <days>` |
| folders scanned | `/Users/<you>` | `stow config add-root` / `remove-root` |

Excludes (never offloaded, to avoid breakage): `~/Library`, `/Applications`,
`.app`/`.photoslibrary` bundles, `~/Library/CloudStorage`, `node_modules`,
`.git`, `DerivedData`, `.venv`, caches, hidden dirs.

**Fallback:** if the Stow folder isn't mounted (agent stopped, extension
disabled), `stow offload` leaves a tiny `STOW1` JSON stub instead of a symlink;
stubs don't auto-download — `stow restore` brings them back, and `stow migrate`
upgrades them to transparent symlinks once the folder is available. While
offloaded, Spotlight still finds a file **by name**; content search resumes
after it's downloaded.

"Last used" is determined primarily by Spotlight's `kMDItemLastUsedDate`, falling
back to `max(atime, mtime)` when Spotlight has no value.

---

## Mode 3 — Permanent share links (`stow share`)

Any file or folder — local, offloaded, or in the Stow folder — can be published
as a **permanent public link**:

```sh
stow share <path>          # publish; link is printed + copied to the clipboard
stow shares                # list active links
stow unshare <token|url>   # revoke (deletes the public copy; the link 404s)
```

Or from the menu bar: **Share a File or Folder…** picks via an open panel and
puts the link on the clipboard.

How it works:

- The content is published under `shares/<128-bit-random-token>/<name>` in your
  bucket. A bucket policy makes **only the `shares/` prefix** publicly readable —
  ACLs stay blocked, listing stays denied, and `objects/` / `fp/` stay private.
- **Already-offloaded files are copied server-side** (S3 `CopyObject`) — sharing
  a 5 GB offloaded file transfers nothing through your Mac.
- **Folders are zipped** (hidden mirror area excluded) and uploaded as one
  archive, so the recipient gets a single download.
- **One stable URL per file**: `stow share` is idempotent — re-running it on the
  same path returns the *same* link (and refreshes the published content), so
  it doubles as "show me this file's URL". A fresh token is only minted after
  `stow unshare` (a revoked link stays dead forever).
- Between shares, links are snapshots: editing a file doesn't change what its
  link serves until you `stow share` it again. Links never expire on their own.

---

## Mode 4 — Auto-clean regenerable caches (`stow clean`)

Package-manager and tool caches (`~/.cache`, `~/.npm`, `~/.bun`, `~/.gradle`,
`~/.cargo/registry`, Hugging Face models, …) pile up fast. These should **not**
be offloaded — stubbing a cache file makes the tool read a placeholder instead of
getting a clean cache *miss*, so it breaks instead of re-fetching. Instead Stow
**deletes** them: every entry is something the tool re-downloads or rebuilds on
demand.

```sh
stow clean                 # dry run: list regenerable caches idle ≥ 30 days
stow clean --days 14       # use a 14-day idle threshold
stow clean --all           # ignore age — every known cache
stow clean --apply         # actually delete them
```

It only touches a **curated allow-list** of known caches (`rust/stow_core/src/cache.rs`).
It never touches source, credentials (`.ssh`/`.aws`/…), installed toolchains
(`.rustup`/`.ghcup`, `.cargo/bin`), or config.

**Automatic:** `stow schedule` installs a weekly LaunchAgent
(`ai.exla.stow.clean`, Sundays 12:30) that runs `stow clean --apply` for caches
idle ≥ 30 days. The clean job runs the **CLI** (not the sandboxed agent), because
the agent's sandbox can't reach `~/.cache` etc.

## Install

```sh
brew tap viraatdas/tap
brew install stow
stow init
# then enable the extension once (Mode 1 setup above) and/or `stow schedule`
```

### Build from source

```sh
make build       # Rust core + xcframework + agent + extension + CLI (ad-hoc)
make install     # install StowAgent.app + stow CLI locally
# signed/notarized build (needs Developer ID + notarytool profile "stow-notary"):
bash scripts/sign-notarize.sh
```

---

## How signing/distribution works

- The CLI is dependency-free and needs no provisioning profile.
- The agent + extension carry restricted entitlements (App Group, File Provider),
  so they need a **Developer ID provisioning profile** that includes the App
  Groups capability, then **notarization + staple**. `scripts/sign-notarize.sh`
  builds unsigned, embeds the profiles (`/tmp/stow_*.provisionprofile`, minted via
  the App Store Connect API), Developer-ID signs, notarizes, staples, installs.
- The Homebrew formula is **build-from-source** (compiles on the user's machine),
  which sidesteps quarantine for the locally-built extension.

---

## Persistence

- `StowAgent` runs at login + KeepAlive via `~/Library/LaunchAgents/ai.exla.stow.agent.plist`
  (`packaging/launchd/`), so the folder + transparency are always available.
- `stow schedule` installs `~/Library/LaunchAgents/ai.exla.stow.auto.plist` for the
  daily whole-account sweep.

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Copy into Stow folder hangs / times out | Extension not enabled → enable in System Settings (Mode 1 setup). Verify `enabled: yes` via `fileproviderctl diagnose`. |
| `addDomain` fails `-2014` | `NSExtensionFileProviderDocumentGroup` must be **inside** the `NSExtension` dict in the extension Info.plist. |
| File op fails `-2011` "Sync is not enabled" | Domain is `user-disabled` — enable the extension toggle. |
| `stow init` hangs | AWS SDK probing EC2 metadata. Stow sets `AWS_EC2_METADATA_DISABLED`; ensure creds are in `~/.aws` or env. |
| Extension can't reach S3 | The sandboxed extension can't read `~/.aws`; `stow init` captures creds into the shared App Group config for it. |
| Extension fails every op with `Operation not permitted` / `unable to open database file` (but the agent works) | App Group container access denied. The App Group ID **must be team-prefixed** (`3C4383262W.ai.exla.stow`) so it matches the provisioning profile's `<TeamID>.*` wildcard — a bare `group.…` id is unauthorized for the *sandboxed* extension (the non-sandboxed paths get a grandfathered grant and mask the bug). |
| Extension `SQLITE_CANTOPEN` on the shared DB | The provider DB must **not** use WAL: a File Provider extension's sandbox blocks WAL's mmap-backed `-shm`. Stow uses `journal_mode=TRUNCATE`. |

---

## Status (what's done vs. open)

**Working & verified:**
- Transparent Stow folder: drop→upload, open→auto-download, byte-identical ✅
- **Auto-eviction of the Stow folder**: hourly sweep evicts ≥10 MB / ≥30-day-
  untouched files to dataless; verified end-to-end (drop → upload → auto-evict →
  dataless → read → byte-identical re-download, with `last_access` reset on read) ✅
- CLI whole-account auto-offload: scan / auto / schedule / restore ✅
- **Transparent in-place offloads (v0.5)**: offload = dataless mirror + symlink;
  opening the original path auto-downloads (verified: 13.4 MB MP3 hydrated
  through `file`/`afinfo` in ~10 s); `stow migrate` upgraded 75 legacy stubs ✅
- **Share links (v0.5)**: `stow share` file/folder→zip, anonymous download
  verified byte-identical, revoke→403, private prefixes stay 403 ✅
- Menu bar status item: space saved at a glance, Open Stow Folder, Share… ✅
- Public Homebrew tap, notarized signing pipeline, app icon, `stow.viraat.dev` ✅

**Open / future:**
- App offloading (`.app` bundles) — not implemented.
- Multi-GB folder shares are zipped in memory — stream to a temp file instead.
