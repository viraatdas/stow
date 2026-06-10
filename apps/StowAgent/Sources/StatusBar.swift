import AppKit
import FileProvider

// A menu bar (status bar) item for the otherwise-faceless agent. It's a read-only
// glance at Stow's headline numbers — disk space currently saved and how many
// files are offloaded — mirroring `stow status`. Refreshes when the menu opens
// and on a slow timer so the bar title stays roughly current.
final class StatusBarController: NSObject, NSMenuDelegate {
    // Created in install() — after the app finishes launching and after the
    // preferred-position default is seeded (see below).
    private var item: NSStatusItem!
    private let menu = NSMenu()
    private let savedItem = NSMenuItem(title: "Stow", action: nil, keyEquivalent: "")
    private let cloudItem = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private var timer: Timer?

    /// Numbers shown in the dropdown. The bar itself stays a bare icon — no
    /// byte count cluttering the menu bar; click to see the figures.
    private struct Summary {
        var savedLine: String     // "Saved 3.8 GB on disk · 75 files offloaded"
        var cloudLine: String     // "75 files, 3.8 GB in S3 · bucket (region)"
    }

    func install() {
        // On notched Macs, macOS can park a brand-new status item *under the
        // notch*: the window exists (observed at x=766 with the notch spanning
        // 771–956) but the hardware physically hides it. Placement honors the
        // autosaved "preferred position" (distance in points from the screen's
        // right edge), so seed one on first run that lands well right of the
        // notch, among the system items. Only seeded when absent — if the user
        // ⌘-drags the icon, macOS overwrites this key and we must not stomp it.
        let posKey = "NSStatusItem Preferred Position StowMenuBar"
        if UserDefaults.standard.object(forKey: posKey) == nil {
            UserDefaults.standard.set(250, forKey: posKey)
        }
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.autosaveName = "StowMenuBar"
        item.isVisible = true
        item.behavior = [] // not user-removable; never auto-hidden by drag-off
        if let button = item.button {
            if let img = NSImage(systemSymbolName: "archivebox",
                                 accessibilityDescription: "Stow") {
                img.isTemplate = true // adapts to light/dark menu bar
                button.image = img
                button.imagePosition = .imageOnly
                button.title = ""
            } else {
                // Symbol unavailable for some reason — text beats invisibility.
                button.title = "Stow"
            }
            stowDiag("status bar: image=\(button.image != nil) frame=\(button.frame)")
        } else {
            stowDiag("status bar: NO BUTTON (item not materialized)")
        }
        // The on-screen position only settles after the window server lays the
        // bar out; log it so "exists but hidden under the notch" is diagnosable
        // from the log alone (button.frame above is view-local and always ~32pt).
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            guard let self, let win = self.item.button?.window else {
                stowDiag("status bar: no window after 2s")
                return
            }
            stowDiag("status bar: window=\(win.frame) onScreen=\(win.isVisible)")
        }

        menu.delegate = self
        savedItem.isEnabled = false
        cloudItem.isEnabled = false
        menu.addItem(savedItem)
        menu.addItem(cloudItem)
        menu.addItem(.separator())

        let open = NSMenuItem(title: "Open Stow Folder",
                              action: #selector(openFolder), keyEquivalent: "")
        open.target = self
        menu.addItem(open)

        let share = NSMenuItem(title: "Share a File or Folder…",
                               action: #selector(shareSomething), keyEquivalent: "s")
        share.target = self
        menu.addItem(share)

        let refreshMI = NSMenuItem(title: "Refresh",
                                   action: #selector(refresh), keyEquivalent: "r")
        refreshMI.target = self
        menu.addItem(refreshMI)

        item.menu = menu
        refresh()
        // Keep the bar title current without the user opening the menu. 5 min is
        // plenty — offload counts only change on a sweep or an explicit CLI call.
        timer = Timer.scheduledTimer(withTimeInterval: 300, repeats: true) { [weak self] _ in
            self?.refresh()
        }
    }

    func menuWillOpen(_ menu: NSMenu) { refresh() }

    @objc private func refresh() {
        // status() touches the DB + filesystem (placeholder checks) — keep it off
        // the main thread, then hop back to mutate UI.
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let summary = StatusBarController.computeSummary()
            DispatchQueue.main.async {
                guard let self else { return }
                self.savedItem.title = summary.savedLine
                self.cloudItem.title = summary.cloudLine
            }
        }
    }

    @objc private func openFolder() {
        // Ask fileproviderd where the domain is mounted (the dir is named
        // "<AppName>-<DomainName>", e.g. StowAgent-Stow — never hardcode it).
        guard let manager = NSFileProviderManager(for: StowDomain.domain) else { return }
        manager.getUserVisibleURL(for: .rootContainer) { url, err in
            guard let url else {
                stowDiag("openFolder: getUserVisibleURL failed: \(String(describing: err))")
                return
            }
            DispatchQueue.main.async {
                // The URL is security-scoped: the sandboxed agent has no access
                // to ~/Library/CloudStorage on its own, so the scope must be
                // active while handing the URL to Finder ("does not have
                // permission to open" otherwise).
                let scoped = url.startAccessingSecurityScopedResource()
                NSWorkspace.shared.open(url)
                if scoped { url.stopAccessingSecurityScopedResource() }
            }
        }
    }

    /// Pick any file or folder → upload (or server-side copy if already
    /// offloaded) → permanent public link on the clipboard. Folders are zipped
    /// so the recipient gets one download.
    @objc private func shareSomething() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Share a file or folder — Stow publishes it and copies a link to your clipboard."
        panel.prompt = "Share"
        NSApp.activate(ignoringOtherApps: true)
        panel.begin { resp in
            guard resp == .OK, let url = panel.url else { return }
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    let r = try StowEngine.share(url.path)
                    let link = r["url"] as? String ?? ""
                    DispatchQueue.main.async {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(link, forType: .string)
                        let a = NSAlert()
                        a.messageText = "Share link copied"
                        a.informativeText = "\(url.lastPathComponent)\n\(link)\n\nAnyone with this link can download it. Revoke with `stow unshare`."
                        a.alertStyle = .informational
                        a.runModal()
                    }
                } catch {
                    DispatchQueue.main.async {
                        let a = NSAlert()
                        a.messageText = "Couldn't share \(url.lastPathComponent)"
                        a.informativeText = "\(error)"
                        a.alertStyle = .warning
                        a.runModal()
                    }
                }
            }
        }
    }

    /// Mirror of `stow status`: "saved on disk" counts only what's currently
    /// freeing space — dataless Stow-folder files plus in-place CLI offloads still
    /// sitting as placeholders. A restored in-place file lives in S3 but saves no
    /// local disk, so it's excluded here (but still counts toward the S3 total).
    private static func computeSummary() -> Summary {
        guard let r = try? StowEngine.status() else {
            return Summary(savedLine: "Stow — not initialized",
                           cloudLine: "Run `stow init` to set up your bucket")
        }
        let cliCount = r["count"] as? Int ?? 0
        let cliBytes = r["bytes_offloaded"] as? Int ?? 0
        let folderCount = r["folder_count"] as? Int ?? 0
        let folderBytes = r["folder_bytes"] as? Int ?? 0

        // DB-derived (sandbox-safe): the agent can't stat the original offload
        // paths, so the core computes these from the index instead.
        let savedBytes = r["saved_bytes"] as? Int ?? 0
        let savedCount = r["saved_count"] as? Int ?? 0
        let cloudCount = cliCount + folderCount
        let cloudBytes = cliBytes + folderBytes

        let bucket = r["bucket"] as? String ?? "?"
        let region = r["region"] as? String ?? "?"

        return Summary(
            savedLine: "Saved \(humanBytes(savedBytes)) on disk · \(savedCount) file\(savedCount == 1 ? "" : "s") offloaded",
            cloudLine: "\(cloudCount) file\(cloudCount == 1 ? "" : "s"), \(humanBytes(cloudBytes)) in S3 · \(bucket) (\(region))")
    }

    /// Human-readable byte size (KB/MB/GB…). Local copy — the agent doesn't link
    /// the CLI's helpers.
    private static func humanBytes(_ n: Int) -> String {
        let units = ["B", "KB", "MB", "GB", "TB"]
        var v = Double(n), i = 0
        while v >= 1024 && i < units.count - 1 { v /= 1024; i += 1 }
        return i == 0 ? "\(n) B" : String(format: "%.1f %@", v, units[i])
    }
}
