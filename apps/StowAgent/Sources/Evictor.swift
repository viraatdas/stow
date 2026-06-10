import FileProvider
import Foundation
import os.log

/// Auto-offload for the transparent Stow folder: on a schedule, evict files that
/// are large, materialized, and haven't been used recently — so they live only
/// in S3 (reading one re-downloads it via the extension's fetchContents).
///
/// We don't need a path→identifier mapping API: the provider DB already stores
/// every item's id, filename and size. We enumerate it, check the on-disk file's
/// materialization (block count) + last-used time, and call
/// `NSFileProviderManager.evictItem(identifier:)` directly. The bytes are already
/// in S3 (uploaded by createItem), so eviction only frees local disk.
final class Evictor {
    private let log = Logger(subsystem: "ai.exla.stow", category: "evictor")
    private var timer: DispatchSourceTimer?
    private var trigger: DispatchSourceFileSystemObject?

    /// Sentinel in the App Group container; touching it forces an immediate sweep
    /// (so the `stow` CLI / a user can offload-now without waiting for the timer).
    private var sentinel: URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: "3C4383262W.ai.exla.stow")?
            .appendingPathComponent("sweep-now")
    }

    /// Start an hourly sweep (first run shortly after launch). Hourly is cheap —
    /// eviction is a no-op for anything already dataless or recently used.
    func start() {
        let t = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        t.schedule(deadline: .now() + 20, repeating: .seconds(3600))
        t.setEventHandler { [weak self] in self?.sweep() }
        t.resume()
        timer = t
        stowDiag("evictor: timer armed (first sweep in ~20s, then hourly)")
        armTrigger()
    }

    /// Watch the sentinel file for writes/touches → force an immediate sweep.
    private func armTrigger() {
        guard let s = sentinel else { return }
        if !FileManager.default.fileExists(atPath: s.path) {
            FileManager.default.createFile(atPath: s.path, contents: Data())
        }
        let fd = open(s.path, O_EVTONLY)
        guard fd >= 0 else { stowDiag("evictor: cannot watch sentinel"); return }
        let src = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd, eventMask: [.write, .attrib, .extend], queue: .global(qos: .utility))
        src.setEventHandler { [weak self] in
            stowDiag("evictor: sweep-now triggered")
            self?.sweep()
        }
        src.setCancelHandler { close(fd) }
        src.resume()
        trigger = src
        stowDiag("evictor: sweep-now sentinel armed at \(s.path)")
    }

    private func sweep() {
        // Policy thresholds from the shared config (fall back to conservative defaults).
        var minSize: Int64 = 10 * 1024 * 1024
        var minAgeDays = 90
        if let cfg = try? StowEngine.getConfig(), let p = cfg["policy"] as? [String: Any] {
            minSize = (p["min_size_bytes"] as? NSNumber)?.int64Value ?? minSize
            minAgeDays = (p["min_age_days"] as? Int) ?? minAgeDays
        }
        let minAge = TimeInterval(minAgeDays * 24 * 3600)
        stowDiag("evictor sweep: minSize=\(minSize) minAgeDays=\(minAgeDays)")

        guard let manager = NSFileProviderManager(for: StowDomain.domain) else {
            stowDiag("evictor: no manager for domain")
            return
        }

        // Every item the provider knows about, from the shared DB — the visible
        // folder tree plus the hidden in-place mirrors (which must re-evict after
        // an app hydrates one by opening its symlink).
        let items: [StowFPItem]
        do {
            items = try StowProvider.enumerate(parentID: "__stow_all__")
        } catch {
            stowDiag("evictor: enumerate failed: \(error.localizedDescription)")
            return
        }
        stowDiag("evictor: \(items.count) item(s) in DB")

        // Staleness uses the DB's last_access (set on create, bumped on every
        // fetchContents) — the sandboxed agent can't stat the user-visible
        // CloudStorage path, so we never read the file on disk. evictItem routes
        // through fileproviderd (not a file path), so it works regardless; it's a
        // harmless no-op on an already-dataless item.
        let nowSecs = Int64(Date().timeIntervalSince1970)
        var evicted = 0
        for it in items where !it.isFolder && it.size >= minSize {
            let ageSecs = nowSecs - it.lastAccess
            let ageDays = Int(ageSecs / 86400)
            if ageSecs < Int64(minAge) {
                stowDiag("evictor: \(it.filename) too recent (\(ageDays)d < \(minAgeDays)d)")
                continue
            }
            stowDiag("evictor: evicting \(it.filename) (size=\(it.size) age=\(ageDays)d) id=\(it.itemID)")
            let ident = NSFileProviderItemIdentifier(it.itemID)
            manager.evictItem(identifier: ident) { err in
                if let err {
                    stowDiag("evictor: evict \(it.filename): \(err.localizedDescription)")
                } else {
                    // Keep the shared DB in sync so `stow status` lists it as
                    // offloaded (fileproviderd won't call back to tell us).
                    try? StowProvider.setDataless(id: it.itemID, true)
                    stowDiag("evictor: auto-offloaded \(it.filename) ✅")
                }
            }
            evicted += 1
        }
        stowDiag("evictor sweep done: \(evicted) candidate(s) evicted")
    }

    /// Public entry so the CLI/agent can force a sweep on demand.
    func sweepNow() { sweep() }
}
