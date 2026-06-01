# Stow — Documentation

Stow offloads files from your Mac to **your own** AWS S3 bucket to free disk
space, and brings them back on demand. It works two complementary ways:

1. **Transparent cloud folder** (Dropbox Smart Sync style) — a **Stow** folder in
   Finder. Files you put there live in S3; opening one downloads it back
   automatically, with no command.
2. **Whole-account auto-offload** (CLI) — a daily background job that offloads
   large, unused files *in place* anywhere under your home directory.

Everything stays in your AWS account. Stow never sees your data. Apple Silicon,
macOS 14+.

---

## Architecture

Four components share one App Group container (`group.ai.exla.stow`), which holds
the config + SQLite indexes that all of them read/write:

| Component | What it is | Role |
|---|---|---|
| `stow` | CLI (`/opt/homebrew/bin/stow`) | user-facing: `init`, `offload`, `restore`, `status`, `scan`, `auto`, `schedule`, `config` |
| `StowAgent.app` | faceless agent (`LSUIElement`, no Dock icon) | registers the File Provider domain, runs the auto-evict sweep; always-on via LaunchAgent |
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

The File Provider folder can't reach files *outside* it (an OS limitation — same
for Dropbox/iCloud). For offloading files that live anywhere in your home dir,
the CLI replaces an unused file **in place** with a tiny placeholder and uploads
the content to S3.

```sh
stow init                 # detect AWS creds, auto-create the S3 bucket, save config
stow scan                 # dry run: what the policy would offload (no changes)
stow auto                 # offload everything matching the policy now
stow schedule [HH:MM]     # run `auto` automatically every day (default 12:00)
stow unschedule           # stop the daily run
stow offload <path>       # offload one specific file
stow restore <path>       # bring an offloaded file back (byte-identical)
stow status               # what's offloaded, space saved
```

**Default policy** (conservative — tune with `stow config`):

| Setting | Default | Change with |
|---|---|---|
| min size | 10 MB | `stow config set-size <MB>` |
| min age (untouched) | 90 days | `stow config set-age <days>` |
| folders scanned | `/Users/<you>` | `stow config add-root` / `remove-root` |

Excludes (never offloaded, to avoid breakage): `~/Library`, `/Applications`,
`.app`/`.photoslibrary` bundles, `~/Library/CloudStorage`, `node_modules`,
`.git`, `DerivedData`, `.venv`, caches, hidden dirs.

**Caveat (CLI mode only):** an in-place offloaded file is a placeholder until you
`stow restore` it — opening it does **not** auto-download (that transparency is
only available inside the Stow folder, Mode 1). While offloaded, Spotlight can
still find it **by name**, but content search is paused until restored.

"Last used" is determined primarily by Spotlight's `kMDItemLastUsedDate`, falling
back to `max(atime, mtime)` when Spotlight has no value.

---

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

---

## Status (what's done vs. open)

**Working & verified:**
- Transparent Stow folder: drop→upload, open→auto-download, byte-identical ✅
- CLI whole-account auto-offload: scan / auto / schedule / restore ✅
- Public Homebrew tap, notarized signing pipeline, app icon, `stow.viraat.dev` ✅

**Open / future:**
- **Auto-eviction of the Stow folder** is currently a stub — the transparent
  download-on-open is fully live, but automatically pushing *in-folder* files to
  dataless on a schedule (to reclaim space) is not yet wired (the
  path→item-identifier eviction API needs finishing). The CLI's scheduled
  `stow auto` handles automatic offload for files outside the folder.
- App offloading (`.app` bundles) — not implemented.
