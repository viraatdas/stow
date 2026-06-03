import Foundation
import CStowCore

/// Manages the launchd agents that make stow "automatic": a daily `stow auto`
/// (offload matching files) and a weekly `stow clean --apply` (reclaim
/// regenerable tool/package caches). Both run the CLI, because cache cleanup
/// touches `~/.cache` etc. which the sandboxed StowAgent can't reach.
enum Scheduler {
    static let label = "ai.exla.stow.auto"
    static let cleanLabel = "ai.exla.stow.clean"

    private static func plistURL(_ label: String) -> URL {
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
        FileManager.default.fileExists(atPath: plistURL(label).path)
    }

    static func isCleanInstalled() -> Bool {
        FileManager.default.fileExists(atPath: plistURL(cleanLabel).path)
    }

    /// Daily offload job (`stow auto`).
    static func install(hour: Int, minute: Int) throws {
        try installJob(label: label, args: [stowPath, "auto"],
                       calendar: ["Hour": hour, "Minute": minute], logName: "stow-auto.log")
    }

    /// Weekly cache-clean job (`stow clean --apply`). Runs Sundays so it doesn't
    /// thrash caches that just crossed the idle threshold.
    static func installClean(weekday: Int = 0, hour: Int = 12, minute: Int = 30) throws {
        try installJob(label: cleanLabel, args: [stowPath, "clean", "--apply"],
                       calendar: ["Weekday": weekday, "Hour": hour, "Minute": minute],
                       logName: "stow-clean.log")
    }

    private static func installJob(label: String, args: [String],
                                   calendar: [String: Int], logName: String) throws {
        let logDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs")
        try? FileManager.default.createDirectory(at: logDir, withIntermediateDirectories: true)
        let logPath = logDir.appendingPathComponent(logName).path

        let url = plistURL(label)
        let plist: [String: Any] = [
            "Label": label,
            "ProgramArguments": args,
            "StartCalendarInterval": calendar,
            "RunAtLoad": false,
            "StandardOutPath": logPath,
            "StandardErrorPath": logPath,
        ]
        let data = try PropertyListSerialization.data(
            fromPropertyList: plist, format: .xml, options: 0)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url)

        // Reload: bootout (ignore if not loaded) then bootstrap into the GUI domain.
        let uid = getuid()
        _ = run("/bin/launchctl", ["bootout", "gui/\(uid)/\(label)"])
        let rc = run("/bin/launchctl", ["bootstrap", "gui/\(uid)", url.path])
        if rc != 0 {
            // Fall back to legacy load for older macOS.
            _ = run("/bin/launchctl", ["load", "-w", url.path])
        }
    }

    static func uninstall() throws { try uninstallJob(label) }
    static func uninstallClean() throws { try uninstallJob(cleanLabel) }

    private static func uninstallJob(_ label: String) throws {
        let uid = getuid()
        _ = run("/bin/launchctl", ["bootout", "gui/\(uid)/\(label)"])
        _ = run("/bin/launchctl", ["unload", plistURL(label).path])
        try? FileManager.default.removeItem(at: plistURL(label))
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
