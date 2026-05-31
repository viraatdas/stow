import FileProvider
import UniformTypeIdentifiers

/// An NSFileProviderItem backed by a Stow File Provider record. The OS reads
/// these to render the Stow folder and to decide what's dataless vs local.
final class StowItem: NSObject, NSFileProviderItem {
    private let model: StowFPItem
    private let isRoot: Bool

    init(_ model: StowFPItem) {
        self.model = model
        self.isRoot = false
        super.init()
    }

    private init(root: Bool) {
        // Root container placeholder.
        self.model = StowFPItem([
            "item_id": NSFileProviderItemIdentifier.rootContainer.rawValue,
            "parent_id": NSFileProviderItemIdentifier.rootContainer.rawValue,
            "filename": "Stow",
            "is_folder": true,
        ])!
        self.isRoot = true
        super.init()
    }

    static var root: StowItem { StowItem(root: true) }

    var itemIdentifier: NSFileProviderItemIdentifier {
        isRoot ? .rootContainer : NSFileProviderItemIdentifier(model.itemID)
    }

    var parentItemIdentifier: NSFileProviderItemIdentifier {
        // The provider stores the root's children under the ROOT sentinel string;
        // map that back to .rootContainer for the OS.
        if model.parentID == NSFileProviderItemIdentifier.rootContainer.rawValue
            || model.parentID == "NSFileProviderRootContainerItemIdentifier" {
            return .rootContainer
        }
        return NSFileProviderItemIdentifier(model.parentID)
    }

    var filename: String { model.filename }

    var contentType: UTType {
        if isRoot || model.isFolder { return .folder }
        return UTType(model.contentType) ?? .data
    }

    var documentSize: NSNumber? {
        (isRoot || model.isFolder) ? nil : NSNumber(value: model.size)
    }

    var childItemCount: NSNumber? {
        (isRoot || model.isFolder) ? nil : NSNumber(value: 0)
    }

    /// Bumped whenever content changes so the OS knows to re-fetch.
    var itemVersion: NSFileProviderItemVersion {
        let v = "\(model.version)".data(using: .utf8)!
        return NSFileProviderItemVersion(contentVersion: v, metadataVersion: v)
    }

    var capabilities: NSFileProviderItemCapabilities {
        if isRoot || model.isFolder {
            return [.allowsReading, .allowsContentEnumerating, .allowsAddingSubItems]
        }
        return [.allowsReading, .allowsWriting, .allowsDeleting, .allowsRenaming, .allowsReparenting]
    }

    /// Lazy download: files stay dataless until first read, and remain evictable.
    var contentPolicy: NSFileProviderContentPolicy { .downloadLazily }

    var contentModificationDate: Date? {
        Date(timeIntervalSince1970: TimeInterval(model.modifiedAt))
    }
}
