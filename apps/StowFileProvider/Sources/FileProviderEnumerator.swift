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
        if container == .rootContainer || container == .workingSet {
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
                observer.didEnumerate(items.map { StowItem($0) })
                observer.finishEnumerating(upTo: nil)
            } catch {
                observer.finishEnumeratingWithError(error)
            }
        }
    }

    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(nil)
    }
}
