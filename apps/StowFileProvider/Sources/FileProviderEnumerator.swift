import FileProvider

/// Enumerates a container's children from the Rust-backed index. The root
/// container maps to the provider's ROOT sentinel.
final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let container: NSFileProviderItemIdentifier
    private let work = DispatchQueue(label: "ai.exla.stow.enum", qos: .userInitiated)

    init(container: NSFileProviderItemIdentifier) {
        self.container = container
        super.init()
    }

    func invalidate() {}

    private var parentKey: String {
        // The working set is "items of interest" — a flat list, not a tree. It
        // must include the hidden in-place mirrors so fileproviderd learns about
        // rows the CLI inserts directly into the DB.
        if container == .workingSet { return "__stow_all__" }
        if container == .rootContainer {
            return NSFileProviderItemIdentifier.rootContainer.rawValue
        }
        return container.rawValue
    }

    func enumerateItems(
        for observer: NSFileProviderEnumerationObserver,
        startingAt page: NSFileProviderPage
    ) {
        work.async {
            do {
                let items = try StowProvider.enumerate(parentID: self.parentKey)
                fpTrace("enumerateItems \(self.parentKey): \(items.count) item(s)")
                observer.didEnumerate(items.map { StowItem($0) })
                observer.finishEnumerating(upTo: nil)
            } catch {
                fpTrace("enumerateItems \(self.parentKey) ERROR \(error)")
                observer.finishEnumeratingWithError(error)
            }
        }
    }

    /// The CLI writes rows straight into the shared DB (in-place mirrors), so we
    /// can't produce a per-item change feed. Instead the anchor fingerprints the
    /// whole table: unchanged → "no changes"; changed → declare the anchor
    /// expired, which makes fileproviderd re-enumerate everything. The tree is
    /// small, so a full re-sync is cheap — and it's the only way DB rows the
    /// extension didn't create become visible (signalEnumerator alone just asks
    /// us "what changed?", and answering "nothing" buries them forever).
    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        work.async {
            let current = (try? StowProvider.anchor()) ?? "0"
            if Data(current.utf8) == anchor.rawValue {
                observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
            } else {
                fpTrace("enumerateChanges \(self.parentKey): anchor moved -> full re-enumeration")
                observer.finishEnumeratingWithError(NSFileProviderError(.syncAnchorExpired))
            }
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        work.async {
            let a = (try? StowProvider.anchor()) ?? "0"
            completionHandler(NSFileProviderSyncAnchor(Data(a.utf8)))
        }
    }
}
