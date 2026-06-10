import Foundation

// The `stow` CLI — the only user-facing surface. A thin client: control actions
// are sent to the faceless StowAgent over a local IPC socket; read-only queries
// can read the shared index directly. M0.5 ships the command tree with stub
// bodies; real behavior lands in M1+.
//
// Deliberately dependency-free (no SwiftPM): SwiftPM's resolver runs in its own
// sandbox-exec, which can't nest inside Homebrew's build sandbox. A tiny built-in
// parser keeps `brew install` (build-from-source) fully self-contained.

// Share config + DBs with the sandboxed agent/extension via the App Group dir.
StowCoreLib.bootstrap()
let coreVersion = StowCoreLib.version()

func stderr(_ s: String) {
    FileHandle.standardError.write(Data((s + "\n").utf8))
}

func printUsage() {
    print("""
    stow — offload unused files on macOS to your own S3.

    USAGE:
      stow <command> [options]

    COMMANDS:
      init                 Auto-provision an S3 bucket in your AWS account
      offload <path>       Offload one file (upload + free disk space;
                           apps auto-download it again on open)
      restore <path>       Pin an offloaded file back on disk, byte-for-byte
      status [-v]          Summary: files offloaded + disk space saved (-v lists files)
      migrate              Upgrade old stub offloads to auto-download on open

      share <path>         Permanent public link for a file (folders → one zip);
                           link is copied to the clipboard
      shares               List active share links
      unshare <token|url>  Revoke a share link (deletes the public copy)

      scan                 Dry run: list files auto-offload would pick (no changes)
      auto                 Offload everything matching the policy now
      clean [--days N] [--apply]  Reclaim regenerable caches (npm/uv/HF/gradle/…)
      schedule [HH:MM]     Run `auto` automatically every day (default 12:00)
      unschedule           Stop the daily automatic run

      config               Show the auto-offload policy
      config set-size <MB> Minimum file size to offload (default 10)
      config set-age <days> Days untouched before offloading (default 90)
      config add-root <dir>     Add a folder to auto-scan
      config remove-root <dir>  Remove a folder from auto-scan
      config include-hidden [on|off]  Scan hidden dirs like ~/.cache (default off)

    OPTIONS:
      --version            Show version
      -h, --help           Show this help
    """)
}

/// Require a positional argument after the subcommand, else error out.
func requireArg(_ args: [String], _ name: String) -> String {
    guard let v = args.first, !v.hasPrefix("-") else {
        stderr("error: '\(name)' is required")
        exit(2)
    }
    return v
}

/// Resolve a possibly-relative path to an absolute path with a stable parent
/// chain — but do NOT resolve the final component: a transparent offload IS a
/// symlink (into the File Provider mirror), and `stow restore`/`status` must
/// address the symlink itself, not its CloudStorage target.
func absolutePath(_ p: String) -> String {
    let url = URL(fileURLWithPath: (p as NSString).expandingTildeInPath).standardizedFileURL
    let parent = url.deletingLastPathComponent().resolvingSymlinksInPath()
    return parent.appendingPathComponent(url.lastPathComponent).path
}

/// Put a string on the clipboard (best-effort; used for share links).
func copyToClipboard(_ s: String) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/pbcopy")
    let pipe = Pipe()
    p.standardInput = pipe
    guard (try? p.run()) != nil else { return }
    pipe.fileHandleForWriting.write(Data(s.utf8))
    pipe.fileHandleForWriting.closeFile()
    p.waitUntilExit()
}

/// Human-readable byte size.
func humanBytes(_ n: Int) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"]
    var v = Double(n), i = 0
    while v >= 1024 && i < units.count - 1 { v /= 1024; i += 1 }
    return i == 0 ? "\(n) B" : String(format: "%.1f %@", v, units[i])
}

func fail(_ e: Error) -> Never {
    stderr("error: \(e)")
    exit(1)
}

let argv = Array(CommandLine.arguments.dropFirst())
guard let command = argv.first else {
    printUsage()
    exit(0)
}
let rest = Array(argv.dropFirst())

switch command {
case "--version":
    print(coreVersion)

case "-h", "--help", "help":
    printUsage()

case "init":
    var region: String? = nil
    if let i = rest.firstIndex(of: "--region"), i + 1 < rest.count { region = rest[i + 1] }
    do {
        let r = try StowEngine.initialize(region: region)
        let bucket = r["bucket"] as? String ?? "?"
        let reg = r["region"] as? String ?? "?"
        let created = r["created"] as? Bool ?? false
        print("✓ AWS account \(r["account"] as? String ?? "?")")
        print("✓ Bucket \(bucket) (\(reg)) \(created ? "created" : "already existed")")
        print("✓ Stow initialized — try: stow offload <file>")
    } catch { fail(error) }

case "add", "offload":
    let path = absolutePath(requireArg(rest, "path"))
    do {
        let r = try StowEngine.offload(path)
        let freed = r["bytes_freed"] as? Int ?? 0
        let deduped = r["deduped"] as? Bool ?? false
        let transparent = r["transparent"] as? Bool ?? false
        print("✓ offloaded \(path)")
        print("  freed \(humanBytes(freed))\(deduped ? " (content already in S3 — deduped)" : "")")
        if transparent {
            print("  opening it downloads it automatically; `stow restore` pins it back")
        } else {
            print("  Stow folder unavailable — left a stub; bring back with: stow restore \(path)")
        }
    } catch { fail(error) }

case "migrate":
    do {
        let r = try StowEngine.migrate()
        let migrated = r["migrated"] as? [String] ?? []
        let skipped = r["skipped"] as? Int ?? 0
        print("✓ upgraded \(migrated.count) offloaded file(s) to auto-download-on-open (\(skipped) already fine)")
        for p in migrated.prefix(20) { print("  ↻ \(p)") }
        if migrated.count > 20 { print("  … and \(migrated.count - 20) more") }
        if let fails = r["failures"] as? [[String: Any]], !fails.isEmpty {
            print("\n\(fails.count) could not be upgraded (still restorable with `stow restore`):")
            for f in fails.prefix(10) {
                print("  \(f["path"] as? String ?? "?"): \(f["error"] as? String ?? "?")")
            }
        }
    } catch { fail(error) }

case "share":
    let path = absolutePath(requireArg(rest, "path"))
    do {
        let r = try StowEngine.share(path)
        let url = r["url"] as? String ?? "?"
        let isFolder = r["is_folder"] as? Bool ?? false
        let size = r["size"] as? Int ?? 0
        let count = r["file_count"] as? Int ?? 1
        copyToClipboard(url)
        if isFolder {
            print("✓ shared folder (\(count) file(s), \(humanBytes(size)) zipped)")
        } else {
            print("✓ shared \(humanBytes(size))")
        }
        print("  \(url)")
        print("  link copied to clipboard — anyone with it can download")
        print("  revoke with: stow unshare \(r["token"] as? String ?? "?")")
    } catch { fail(error) }

case "shares":
    do {
        let r = try StowEngine.listShares()
        let shares = r["shares"] as? [[String: Any]] ?? []
        if shares.isEmpty {
            print("No active share links. Create one with: stow share <path>")
            break
        }
        print("\(shares.count) active share link(s):\n")
        for s in shares {
            let kind = (s["is_folder"] as? Bool ?? false) ? "folder (zip)" : "file"
            print("  \(humanBytes(s["size"] as? Int ?? 0))\t\(kind)\t\(s["source"] as? String ?? "?")")
            print("    \(s["url"] as? String ?? "?")")
            print("    revoke: stow unshare \(s["token"] as? String ?? "?")")
        }
    } catch { fail(error) }

case "unshare":
    var token = requireArg(rest, "token")
    // Accept the full URL too: …/shares/<token>/<name>
    if token.contains("/shares/") {
        let parts = token.components(separatedBy: "/shares/")
        token = parts.last?.components(separatedBy: "/").first ?? token
    }
    do {
        let r = try StowEngine.unshare(token)
        print("✓ revoked share for \(r["source"] as? String ?? "?") — the link is dead")
    } catch { fail(error) }

case "restore":
    let path = absolutePath(requireArg(rest, "path"))
    do {
        let r = try StowEngine.restore(path)
        print("✓ restored \(path) (\(humanBytes(r["bytes_restored"] as? Int ?? 0)))")
    } catch { fail(error) }

case "status":
    // Default: a rough summary (files offloaded + disk space saved). Pass
    // -v/--verbose to also list every file.
    let verbose = rest.contains("-v") || rest.contains("--verbose") || rest.contains("--all")
    do {
        let r = try StowEngine.status()
        let cliCount = r["count"] as? Int ?? 0
        let cliBytes = r["bytes_offloaded"] as? Int ?? 0
        let folderCount = r["folder_count"] as? Int ?? 0
        let folderBytes = r["folder_bytes"] as? Int ?? 0
        let items = r["items"] as? [[String: Any]] ?? []

        // "Saved on disk" counts only what's currently offloaded: dataless Stow
        // folder files plus in-place CLI offloads not yet restored. A restored
        // (●) file is back on disk — its bytes live in S3 but save no local space.
        // Computed by the core from the index (DB), so it matches the menu bar.
        let savedBytes = r["saved_bytes"] as? Int ?? 0
        let savedCount = r["saved_count"] as? Int ?? 0
        let cloudCount = cliCount + folderCount
        let cloudBytes = cliBytes + folderBytes

        // The rough summary you glance at.
        print("Stow — saved \(humanBytes(savedBytes)) on disk, \(savedCount) file(s) offloaded")
        print("bucket: \(r["bucket"] as? String ?? "?") (\(r["region"] as? String ?? "?")) · \(cloudCount) file(s), \(humanBytes(cloudBytes)) in S3")

        if !verbose {
            if cloudCount > 0 { print("\nRun `stow status -v` to list files.") }
            break
        }

        // Transparent Stow folder — files auto-offloaded to dataless (open one to
        // download it back automatically).
        if let f = r["folder_items"] as? [[String: Any]], !f.isEmpty {
            print("\n  Stow folder (auto, downloads on open):")
            for it in f {
                print("    ● \(humanBytes(it["size"] as? Int ?? 0))\t\(it["filename"] as? String ?? "?")")
            }
        }

        // Whole-account, in-place offloads (open one to download it; `stow
        // restore <path>` pins it back permanently).
        if !items.isEmpty {
            print("\n  In-place (downloads on open; `stow restore` to pin back):")
            for it in items {
                let off = (it["present_as_placeholder"] as? Bool ?? false) ? "○" : "●"
                print("    \(off) \(humanBytes(it["size"] as? Int ?? 0))\t\(it["path"] as? String ?? "?")")
            }
            print("\n  ○ = offloaded   ● = restored/local")
        }
    } catch { fail(error) }

case "scan":
    do {
        let r = try StowEngine.scan()
        let count = r["candidate_count"] as? Int ?? 0
        let total = r["reclaimable_bytes"] as? Int ?? 0
        let minMB = (r["min_size_bytes"] as? Int ?? 0) / (1024 * 1024)
        let age = r["min_age_days"] as? Int ?? 0
        let roots = (r["roots"] as? [String])?.joined(separator: ", ") ?? "?"
        print("policy: files ≥ \(minMB) MB, untouched ≥ \(age) days, in \(roots)")
        print("found: \(count) candidate(s), \(humanBytes(total)) reclaimable\n")
        if let cands = r["candidates"] as? [[String: Any]], !cands.isEmpty {
            for c in cands.prefix(40) {
                let d = c["days_unused"] as? Int ?? 0
                print("  \(humanBytes(c["size"] as? Int ?? 0))\t\(d)d unused\t\(c["path"] as? String ?? "?")")
            }
            if cands.count > 40 { print("  … and \(cands.count - 40) more") }
            print("\nThese are NOT offloaded yet. Run `stow auto` to offload them,")
            print("or `stow schedule` to do it automatically every day.")
        } else {
            print("Nothing matches the policy right now.")
        }
    } catch { fail(error) }

case "auto":
    do {
        let r = try StowEngine.auto()
        let n = r["offloaded_count"] as? Int ?? 0
        let freed = r["bytes_freed"] as? Int ?? 0
        print("✓ offloaded \(n) file(s), freed \(humanBytes(freed))")
        if let fails = r["failures"] as? [[String: Any]], !fails.isEmpty {
            print("\n\(fails.count) skipped:")
            for f in fails.prefix(10) {
                print("  \(f["path"] as? String ?? "?"): \(f["error"] as? String ?? "?")")
            }
        }
        if n > 0 { print("\nRestore any of them with: stow restore <path>") }
    } catch { fail(error) }

case "clean":
    // Reclaim regenerable tool/package caches. Dry run unless --apply/-y.
    // `--days N` sets the idle threshold (default 30); `--all` ignores age.
    do {
        let apply = rest.contains("--apply") || rest.contains("-y")
        var minIdle: UInt64 = 30
        if rest.contains("--all") { minIdle = 0 }
        if let i = rest.firstIndex(of: "--days"), i + 1 < rest.count, let d = UInt64(rest[i + 1]) {
            minIdle = d
        }
        let r = try StowEngine.cleanCaches(minIdleDays: minIdle, apply: apply)
        let entries = (r["entries"] as? [[String: Any]]) ?? []
        let reclaimable = r["reclaimable_bytes"] as? Int ?? 0
        let freed = r["freed_bytes"] as? Int ?? 0
        let ageNote = minIdle == 0 ? "all ages" : "idle ≥ \(minIdle) days"
        if entries.isEmpty {
            print("No regenerable caches \(ageNote) found. Nothing to clean.")
        } else if apply {
            print("✓ cleaned \(entries.filter { ($0["removed"] as? Bool ?? false) }.count) cache(s), freed \(humanBytes(freed))\n")
            for e in entries {
                let mark = (e["removed"] as? Bool ?? false) ? "✓" : "·"
                print("  \(mark) \(humanBytes(e["size_bytes"] as? Int ?? 0))\t\(e["name"] as? String ?? "?")")
            }
            print("\nThese regenerate on demand (re-downloaded/rebuilt by their tools).")
        } else {
            print("Regenerable caches \(ageNote) — \(humanBytes(reclaimable)) reclaimable:\n")
            for e in entries {
                let d = e["idle_days"] as? Int ?? 0
                print("  \(humanBytes(e["size_bytes"] as? Int ?? 0))\t\(d)d idle\t\(e["name"] as? String ?? "?")")
                print("           \(e["path"] as? String ?? "?")  (\(e["regenerates"] as? String ?? "regenerates"))")
            }
            print("\nThis is a dry run. Run `stow clean --apply` to delete them")
            print("(safe — each is re-fetched/rebuilt by its tool on next use).")
        }
    } catch { fail(error) }

case "schedule":
    // Optional HH:MM (default 12:00)
    var hour = 12, minute = 0
    if let t = rest.first(where: { $0.contains(":") }) {
        let parts = t.split(separator: ":")
        if parts.count == 2, let h = Int(parts[0]), let m = Int(parts[1]) { hour = h; minute = m }
    }
    do {
        try Scheduler.install(hour: hour, minute: minute)
        try Scheduler.installClean()
        print(String(format: "✓ Stow will auto-offload daily at %02d:%02d.", hour, minute))
        print("  It runs `stow auto` (policy: see `stow config`).")
        print("✓ Stow will auto-clean regenerable caches weekly (Sun 12:30).")
        print("  It runs `stow clean --apply`. Stop both with `stow unschedule`.")
    } catch { fail(error) }

case "unschedule":
    do {
        try Scheduler.uninstall()
        try Scheduler.uninstallClean()
        print("✓ Automatic offloading and cache-cleanup disabled.")
    } catch { fail(error) }

case "config":
    let sub = rest.first
    do {
        switch sub {
        case "set-size":
            let mb = Int(requireArg(Array(rest.dropFirst()), "MB")) ?? -1
            guard mb >= 1 else { stderr("error: size must be ≥ 1 MB"); exit(2) }
            try Policy.update { $0["min_size_bytes"] = mb * 1024 * 1024 }
            print("✓ minimum size set to \(mb) MB")
        case "set-age":
            let days = Int(requireArg(Array(rest.dropFirst()), "days")) ?? -1
            guard days >= 1 else { stderr("error: age must be ≥ 1 day"); exit(2) }
            try Policy.update { $0["min_age_days"] = days }
            print("✓ minimum age set to \(days) days")
        case "add-root":
            let dir = absolutePath(requireArg(Array(rest.dropFirst()), "dir"))
            try Policy.update {
                var roots = ($0["roots"] as? [String]) ?? []
                if !roots.contains(dir) { roots.append(dir) }
                $0["roots"] = roots
            }
            print("✓ added scan folder: \(dir)")
        case "remove-root":
            let dir = absolutePath(requireArg(Array(rest.dropFirst()), "dir"))
            try Policy.update {
                let roots = (($0["roots"] as? [String]) ?? []).filter { $0 != dir }
                $0["roots"] = roots
            }
            print("✓ removed scan folder: \(dir)")
        case "include-hidden":
            let arg = (Array(rest.dropFirst()).first ?? "on").lowercased()
            let on = !(arg == "off" || arg == "false" || arg == "0" || arg == "no")
            try Policy.update { $0["include_hidden"] = on }
            print("✓ scan hidden dirs (e.g. ~/.cache): \(on ? "ON" : "off")")
            if on {
                print("  note: offloaded files auto-download when an app opens them.")
                print("  Credential dirs (.ssh/.aws/…) and .git/.Trash stay protected.")
            }
        default:
            let cfg = try StowEngine.getConfig()
            print("bucket: \(cfg["bucket"] as? String ?? "?") (\(cfg["region"] as? String ?? "?"))")
            if let p = cfg["policy"] as? [String: Any] {
                let minMB = (p["min_size_bytes"] as? Int ?? 0) / (1024 * 1024)
                print("\nauto-offload policy:")
                print("  min size:  \(minMB) MB")
                print("  min age:   \(p["min_age_days"] as? Int ?? 0) days untouched")
                print("  folders:   \((p["roots"] as? [String])?.joined(separator: ", ") ?? "?")")
                print("  hidden:    \((p["include_hidden"] as? Bool ?? false) ? "scanned (~/.cache etc.)" : "skipped (default)")")
            }
            print("\noffload schedule:  \(Scheduler.isInstalled() ? "ON (daily)" : "off — enable with `stow schedule`")")
            print("cache cleanup:     \(Scheduler.isCleanInstalled() ? "ON (weekly)" : "off — enable with `stow schedule`")")
        }
    } catch { fail(error) }

default:
    stderr("error: unknown command '\(command)'")
    printUsage()
    exit(2)
}
