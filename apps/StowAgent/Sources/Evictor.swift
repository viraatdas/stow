import FileProvider
import Foundation
import os.log

/// Periodically frees local disk by evicting Stow-folder files that are
/// materialized but haven't been used recently — the "auto-offload" for the
/// transparent File Provider folder. Files stay in S3; reading one re-downloads
/// it via the extension's fetchContents.
///
/// Self-contained: walks the user-visible folder, and asks the OS to map each
/// stale, large, still-materialized file to its item identifier, then evicts it.
/// Bytes are already safely in S3 (uploaded by createItem), so eviction is a
/// pure space optimization.
final class Evictor {
    private let log = Logger(subsystem: "ai.exla.stow", category: "evictor")
    private var timer: DispatchSourceTimer?

    // Conservative defaults; mirror the CLI policy.
    private let minSize: Int64 = 10 * 1024 * 1024
    private let minAge: TimeInterval = 90 * 24 * 3600

    private var root: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/CloudStorage/StowAgent-Stow")
    }

    /// Start a daily sweep (first run shortly after launch).
    func start() {
        let t = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        t.schedule(deadline: .now() + 120, repeating: .seconds(24 * 3600))
        t.setEventHandler { [weak self] in self?.sweep() }
        t.resume()
        timer = t
    }

    private func sweep() {
        guard let manager = NSFileProviderManager(for: StowDomain.domain) else { return }
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(
            at: root,
            includingPropertiesForKeys: [.fileAllocatedSizeKey, .totalFileSizeKey,
                                         .contentModificationDateKey, .isDirectoryKey],
            options: [.skipsHiddenFiles]
        ) else { return }

        let now = Date()
        for url in entries {
            let vals = try? url.resourceValues(forKeys: [.fileAllocatedSizeKey, .totalFileSizeKey,
                                                         .contentModificationDateKey, .isDirectoryKey])
            if vals?.isDirectory == true { continue }
            // Logical size big enough to be worth offloading?
            let logical = Int64(vals?.totalFileSize ?? 0)
            if logical < minSize { continue }
            // Already dataless (no local blocks)? skip.
            if (vals?.fileAllocatedSize ?? 0) == 0 { continue }
            // Recently modified? leave it.
            let modified = vals?.contentModificationDate ?? .distantPast
            if now.timeIntervalSince(modified) < minAge { continue }

            manager.getIdentifierForUserVisibleItem(at: url) { [weak self] ident, _, error in
                guard let self else { return }
                guard let ident, error == nil else { return }
                manager.evictItem(identifier: ident) { err in
                    if let err {
                        self.log.error("evict failed: \(err.localizedDescription, privacy: .public)")
                    } else {
                        self.log.info("auto-offloaded \(url.lastPathComponent, privacy: .public)")
                    }
                }
            }
        }
    }
}
