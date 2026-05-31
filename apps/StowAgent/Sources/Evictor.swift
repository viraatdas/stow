import FileProvider
import Foundation
import os.log

/// Auto-offload for the transparent Stow folder: periodically evict large,
/// stale, still-materialized files so they live only in S3 (reading one
/// re-downloads it via the extension).
///
/// NOTE: the path→item-identifier mapping API needed to drive
/// `NSFileProviderManager.evictItem` from the agent is still being finalized for
/// this SDK; the sweep is currently a safe no-op stub. The CLI's scheduled
/// `stow auto` already provides automatic offloading for regular folders, and
/// per-file eviction in the Stow folder can be triggered manually meanwhile.
final class Evictor {
    private let log = Logger(subsystem: "ai.exla.stow", category: "evictor")
    private var timer: DispatchSourceTimer?

    /// Start a daily sweep (first run shortly after launch).
    func start() {
        let t = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        t.schedule(deadline: .now() + 120, repeating: .seconds(24 * 3600))
        t.setEventHandler { [weak self] in self?.sweep() }
        t.resume()
        timer = t
    }

    private func sweep() {
        // Intentionally a no-op for now (see note above).
        log.debug("evictor sweep tick")
    }
}
