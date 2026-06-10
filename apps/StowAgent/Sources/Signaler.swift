import FileProvider
import Foundation

/// Bridges DB writes from the (unsandboxed) CLI to fileproviderd. The CLI can't
/// call NSFileProviderManager itself, so after inserting File Provider rows
/// (e.g. the dataless mirrors behind transparent in-place offloads) it touches
/// the `fp-signal` sentinel in the App Group container. We watch that file and
/// call `signalEnumerator` so fileproviderd re-enumerates and materializes the
/// new (dataless) entries — which is what the CLI's symlinks point at.
final class Signaler {
    private var trigger: DispatchSourceFileSystemObject?

    private var sentinel: URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: "3C4383262W.ai.exla.stow")?
            .appendingPathComponent("fp-signal")
    }

    func start() {
        guard let s = sentinel else { stowDiag("signaler: no group container"); return }
        if !FileManager.default.fileExists(atPath: s.path) {
            FileManager.default.createFile(atPath: s.path, contents: Data())
        }
        let fd = open(s.path, O_EVTONLY)
        guard fd >= 0 else { stowDiag("signaler: cannot watch sentinel"); return }
        let src = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd, eventMask: [.write, .attrib, .extend, .delete, .rename],
            queue: .global(qos: .userInitiated))
        src.setEventHandler { [weak self] in
            stowDiag("signaler: fp-signal touched — signaling enumerators")
            self?.signal()
            self?.rearmIfReplaced()
        }
        src.setCancelHandler { close(fd) }
        src.resume()
        trigger = src
        stowDiag("signaler: armed at \(s.path)")
        // Catch up on anything written while the agent was down.
        signal()
    }

    /// `fs::write` replaces the file on some paths — if the watched inode went
    /// away, re-arm on the new file.
    private func rearmIfReplaced() {
        guard let s = sentinel, !FileManager.default.fileExists(atPath: s.path) else { return }
        trigger?.cancel()
        trigger = nil
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.5) { [weak self] in self?.start() }
    }

    /// Tell fileproviderd that our containers changed: the working set (how it
    /// learns about new items), the root, and the hidden in-place mirror folder.
    private func signal() {
        guard let manager = NSFileProviderManager(for: StowDomain.domain) else {
            stowDiag("signaler: no manager for domain")
            return
        }
        let containers: [NSFileProviderItemIdentifier] = [
            .workingSet, .rootContainer, NSFileProviderItemIdentifier("stow-inplace-root"),
        ]
        for c in containers {
            manager.signalEnumerator(for: c) { err in
                if let err {
                    stowDiag("signaler: signal \(c.rawValue): \(err.localizedDescription)")
                }
            }
        }
    }
}
