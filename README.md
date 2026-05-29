# stow — offload your files on mac

Stow transparently offloads files (and, experimentally, apps) you aren't using to
cheap cloud storage, and rehydrates them the instant any process reads them. Files
never disappear — an offloaded file still shows up in Finder, just at ~0 bytes on
disk, and materializes from the cloud on first access.

Apple Silicon only. Clean by design: notarizable, no kernel extension, no SIP /
reduced-security changes. Built on Apple's File Provider framework.

## How it works

Offloaded items live in a managed **Stow** location in Finder's sidebar
(`~/Library/CloudStorage/Stow…`). When a process reads a dataless file, macOS calls
our File Provider extension, which streams the bytes back from **AWS S3** (Standard
storage class) as fast as the link allows, and the read completes transparently.

CLI-first: you drive everything through the `stow` command; a faceless background
agent hosts the File Provider extension. Installed via Homebrew (build-from-source
for now), hosted on `stow.viraat.dev`.

```sh
brew tap viraat/tap
brew install stow      # build-from-source (ad-hoc signed, personal use)
stow init              # auto-detects ~/.aws, creates the bucket, registers the Stow folder
stow add ~/big.mov     # manage a file
stow offload --now     # push eligible files to the cloud (dataless)
stow status            # what's offloaded, space saved
```

## Architecture

Four components sharing one App Group container (`group.ai.exla.stow`):

| Component | What it is | Role |
|---|---|---|
| `stow` | command-line tool (ArgumentParser) | the only user-facing surface; IPC client to the agent |
| `StowAgent.app` | faceless agent (`LSUIElement`, no UI) | control plane: domain registration, eviction engine, CLI IPC server; hosts the extension |
| `StowFileProvider.appex` | `NSFileProviderReplicatedExtension` | hot path: metadata + `fetchContents` rehydration |
| `libstow_core` | Rust static lib → `StowCore.xcframework` | S3 I/O, SQLite index, blake3, AES-256-GCM, parallel ranged downloads |

```
stow/
├─ cli/stow/                # `stow` CLI
├─ apps/StowAgent/          # faceless background agent
├─ apps/StowFileProvider/   # File Provider extension
├─ packages/StowCore/       # Swift wrapper over the Rust C FFI
├─ rust/stow_core/          # Rust core (cargo, staticlib)
├─ packaging/homebrew/      # Homebrew tap (Formula/stow.rb)
├─ scripts/                 # build-rust-xcframework.sh
└─ artifacts/               # built StowCore.xcframework (gitignored)
```

## Building from source (local dev)

```sh
make test     # Rust core tests
make build    # xcframework + agent (with embedded extension) + stow CLI, ad-hoc signed
make install  # StowAgent.app -> ~/Applications, stow -> ~/.local/bin
```

## Status

Greenfield. See the plan in `~/.claude/plans/` for full milestones
(M0 scaffolding → M1 file MVP → M2 eviction → M3 hardening → M4 polish → M5 apps).
