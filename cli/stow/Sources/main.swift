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
      offload <path>       Offload one file (upload + free disk space)
      restore <path>       Bring an offloaded file back, byte-for-byte
      status               Show what's offloaded and space saved

      scan                 Dry run: list files auto-offload would pick (no changes)
      auto                 Offload everything matching the policy now
      schedule [HH:MM]     Run `auto` automatically every day (default 12:00)
      unschedule           Stop the daily automatic run

      config               Show the auto-offload policy
      config set-size <MB> Minimum file size to offload (default 10)
      config set-age <days> Days untouched before offloading (default 90)
      config add-root <dir>     Add a folder to auto-scan
      config remove-root <dir>  Remove a folder from auto-scan

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

/// Resolve a possibly-relative path to an absolute, symlink-resolved path so the
/// engine's index keys are stable.
func absolutePath(_ p: String) -> String {
    let url = URL(fileURLWithPath: (p as NSString).expandingTildeInPath)
    return url.standardizedFileURL.resolvingSymlinksInPath().path
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
        print("✓ offloaded \(path)")
        print("  freed \(humanBytes(freed))\(deduped ? " (content already in S3 — deduped)" : "")")
        print("  restore with: stow restore \(path)")
    } catch { fail(error) }

case "restore":
    let path = absolutePath(requireArg(rest, "path"))
    do {
        let r = try StowEngine.restore(path)
        print("✓ restored \(path) (\(humanBytes(r["bytes_restored"] as? Int ?? 0)))")
    } catch { fail(error) }

case "status":
    do {
        let r = try StowEngine.status()
        let count = r["count"] as? Int ?? 0
        let total = r["bytes_offloaded"] as? Int ?? 0
        print("bucket: \(r["bucket"] as? String ?? "?") (\(r["region"] as? String ?? "?"))")
        print("offloaded: \(count) file(s), \(humanBytes(total)) in the cloud")
        if let items = r["items"] as? [[String: Any]], !items.isEmpty {
            print("")
            for it in items {
                let off = (it["present_as_placeholder"] as? Bool ?? false) ? "○" : "●"
                print("  \(off) \(humanBytes(it["size"] as? Int ?? 0))\t\(it["path"] as? String ?? "?")")
            }
            print("\n  ○ = offloaded (placeholder)   ● = restored/local")
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

case "schedule":
    // Optional HH:MM (default 12:00)
    var hour = 12, minute = 0
    if let t = rest.first(where: { $0.contains(":") }) {
        let parts = t.split(separator: ":")
        if parts.count == 2, let h = Int(parts[0]), let m = Int(parts[1]) { hour = h; minute = m }
    }
    do {
        try Scheduler.install(hour: hour, minute: minute)
        print(String(format: "✓ Stow will auto-offload daily at %02d:%02d.", hour, minute))
        print("  It runs `stow auto` (policy: see `stow config`). Stop with `stow unschedule`.")
        print("  Tip: run `stow scan` first to preview what it'll pick.")
    } catch { fail(error) }

case "unschedule":
    do {
        try Scheduler.uninstall()
        print("✓ Automatic offloading disabled.")
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
        default:
            let cfg = try StowEngine.getConfig()
            print("bucket: \(cfg["bucket"] as? String ?? "?") (\(cfg["region"] as? String ?? "?"))")
            if let p = cfg["policy"] as? [String: Any] {
                let minMB = (p["min_size_bytes"] as? Int ?? 0) / (1024 * 1024)
                print("\nauto-offload policy:")
                print("  min size:  \(minMB) MB")
                print("  min age:   \(p["min_age_days"] as? Int ?? 0) days untouched")
                print("  folders:   \((p["roots"] as? [String])?.joined(separator: ", ") ?? "?")")
            }
            print("\nschedule:  \(Scheduler.isInstalled() ? "ON (daily)" : "off — enable with `stow schedule`")")
        }
    } catch { fail(error) }

default:
    stderr("error: unknown command '\(command)'")
    printUsage()
    exit(2)
}
