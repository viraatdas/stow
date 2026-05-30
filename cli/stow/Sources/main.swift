import Foundation

// The `stow` CLI — the only user-facing surface. A thin client: control actions
// are sent to the faceless StowAgent over a local IPC socket; read-only queries
// can read the shared index directly. M0.5 ships the command tree with stub
// bodies; real behavior lands in M1+.
//
// Deliberately dependency-free (no SwiftPM): SwiftPM's resolver runs in its own
// sandbox-exec, which can't nest inside Homebrew's build sandbox. A tiny built-in
// parser keeps `brew install` (build-from-source) fully self-contained.

let coreVersion = StowCoreLib.version()

func stderr(_ s: String) {
    FileHandle.standardError.write(Data((s + "\n").utf8))
}

func printUsage() {
    print("""
    stow — offload unused files on macOS; rehydrate transparently on access.

    USAGE:
      stow <command> [options]

    COMMANDS:
      init                 Set up Stow: auto-provision S3 and register the Stow folder
      add <path>           Manage a file or folder with Stow
      status               Show offload status and space saved
      offload [--now]      Offload eligible files to the cloud
      restore <path>       Force-rehydrate an offloaded file to local
      config               View or change Stow configuration

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

case "config":
    print("core: v\(coreVersion)")
    do {
        let r = try StowEngine.status()
        print("bucket: \(r["bucket"] as? String ?? "?")")
        print("region: \(r["region"] as? String ?? "?")")
    } catch {
        print("not initialized — run `stow init`")
    }

default:
    stderr("error: unknown command '\(command)'")
    printUsage()
    exit(2)
}
