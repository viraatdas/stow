import AppKit
import os.log

// Diagnostic line to stderr AND a log file (so it's visible regardless of how
// the agent is launched — `open`/launchd send stderr to /dev/null).
func stowDiag(_ s: String) {
    let line = "STOW-AGENT: \(s)\n"
    fputs(line, stderr)
    // NSHomeDirectory() works whether sandboxed (-> container) or not (-> ~).
    let path = NSHomeDirectory() + "/stow-agent-diag.log"
    if let h = FileHandle(forWritingAtPath: path) {
        h.seekToEndOfFile(); h.write(Data(line.utf8)); h.closeFile()
    } else {
        try? line.write(toFile: path, atomically: true, encoding: .utf8)
    }
}

// Stow's faceless background agent. No Dock icon, no menu bar, no windows — it
// exists solely to host the File Provider extension and run the control plane:
// domain registration, the eviction engine, and the local IPC endpoint that the
// `stow` CLI talks to. The user never interacts with it directly.
//
// M0.5: it just launches and proves the Rust core links into the agent process.
// M1 adds NSFileProviderManager domain registration + the IPC server.

final class AgentDelegate: NSObject, NSApplicationDelegate {
    private let log = Logger(subsystem: "ai.exla.stow", category: "agent")

    func applicationDidFinishLaunching(_ notification: Notification) {
        log.info("StowAgent launched; core v\(StowCoreLib.version(), privacy: .public)")
        stowDiag("launched; core v\(StowCoreLib.version())")
        // Register the Stow domain so "Stow" appears in Finder's sidebar.
        StowDomain.register()
        // M1 (next): bootstrap config from the App Group container, start the
        // Unix-domain-socket IPC server for the `stow` CLI, start the eviction
        // engine (free-space watcher + scheduled scan).
    }
}

let app = NSApplication.shared
let delegate = AgentDelegate()
app.delegate = delegate
// .accessory: runs without a Dock icon or menu bar, but can still show panels if
// ever needed. Combined with LSUIElement in Info.plist for a fully faceless agent.
app.setActivationPolicy(.accessory)
app.run()
