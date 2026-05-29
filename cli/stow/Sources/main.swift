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
    print("stow init — not implemented yet (M1): will detect ~/.aws, create the bucket, and register the domain.")

case "add":
    let path = requireArg(rest, "path")
    print("stow add \(path) — not implemented yet (M1): will import into the Stow domain and upload to S3.")

case "status":
    print("stow status — not implemented yet (M1): will read the shared index for tier/size/space-saved.")

case "offload":
    let now = rest.contains("--now")
    print("stow offload\(now ? " --now" : "") — not implemented yet (M1/M2): will evict to dataless after verified upload.")

case "restore":
    let path = requireArg(rest, "path")
    print("stow restore \(path) — not implemented yet (M1): will materialize via the File Provider.")

case "config":
    print("stow config — not implemented yet (M1).")
    print("core: v\(coreVersion)")

default:
    stderr("error: unknown command '\(command)'")
    printUsage()
    exit(2)
}
