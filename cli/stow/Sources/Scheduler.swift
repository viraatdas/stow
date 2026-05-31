import Foundation
import CStowCore

/// Manages the launchd agent that runs `stow auto` on a daily schedule.
/// This is what makes offloading "automatic": a per-user LaunchAgent that wakes
/// once a day, runs the policy, and offloads matching files.
enum Scheduler {
    static let label = "ai.exla.stow.auto"

    private static var plistURL: URL {
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home.appendingPathComponent("Library/LaunchAgents/\(label).plist")
    }

    /// Absolute path to this running `stow` binary, so the agent invokes the
    /// same one the user installed.
    private static var stowPath: String {
        if let p = Bundle.main.executablePath { return p }
        return CommandLine.arguments.first ?? "/opt/homebrew/bin/stow"
    }

    static func isInstalled() -> Bool {
        FileManager.default.fileExists(atPath: plistURL.path)
    }

    static func install(hour: Int, minute: Int) throws {
        let logDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs")
        try? FileManager.default.createDirectory(at: logDir, withIntermediateDirectories: true)
        let logPath = logDir.appendingPathComponent("stow-auto.log").path

        let plist: [String: Any] = [
            "Label": label,
            "ProgramArguments": [stowPath, "auto"],
            "StartCalendarInterval": ["Hour": hour, "Minute": minute],
            "RunAtLoad": false,
            "StandardOutPath": logPath,
            "StandardErrorPath": logPath,
        ]
        let data = try PropertyListSerialization.data(
            fromPropertyList: plist, format: .xml, options: 0)
        try FileManager.default.createDirectory(
            at: plistURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: plistURL)

        // Reload: bootout (ignore if not loaded) then bootstrap into the GUI domain.
        let uid = getuid()
        _ = run("/bin/launchctl", ["bootout", "gui/\(uid)/\(label)"])
        let rc = run("/bin/launchctl", ["bootstrap", "gui/\(uid)", plistURL.path])
        if rc != 0 {
            // Fall back to legacy load for older macOS.
            _ = run("/bin/launchctl", ["load", "-w", plistURL.path])
        }
    }

    static func uninstall() throws {
        let uid = getuid()
        _ = run("/bin/launchctl", ["bootout", "gui/\(uid)/\(label)"])
        _ = run("/bin/launchctl", ["unload", plistURL.path])
        try? FileManager.default.removeItem(at: plistURL)
    }

    @discardableResult
    private static func run(_ tool: String, _ args: [String]) -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do { try p.run(); p.waitUntilExit(); return p.terminationStatus }
        catch { return -1 }
    }
}

/// Read-modify-write helper for the policy block of the config.
enum Policy {
    /// Load the current policy, let the caller mutate it, then persist.
    static func update(_ mutate: (inout [String: Any]) -> Void) throws {
        let cfg = try StowEngine.getConfig()
        var policy = (cfg["policy"] as? [String: Any]) ?? [:]
        mutate(&policy)
        let data = try JSONSerialization.data(withJSONObject: policy)
        let json = String(data: data, encoding: .utf8) ?? "{}"
        try StowEngine.setPolicy(json)
    }
}
