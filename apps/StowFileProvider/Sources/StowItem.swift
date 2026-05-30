import FileProvider
import UniformTypeIdentifiers

// Minimal NSFileProviderItem. M0.5/M1-start: enough to represent the root
// container so a freshly registered domain shows as a valid (empty) Stow folder.
// File items (with documentSize, contentVersion, s3 metadata, dataless state)
// are built from the SQLite index later in M1.
final class StowItem: NSObject, NSFileProviderItem {
    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    let contentType: UTType
    let capabilities: NSFileProviderItemCapabilities

    /// The root container of the Stow domain.
    static var root: StowItem {
        StowItem(
            identifier: .rootContainer,
            parent: .rootContainer,
            filename: "Stow",
            contentType: .folder,
            capabilities: [.allowsReading, .allowsContentEnumerating, .allowsAddingSubItems]
        )
    }

    init(
        identifier: NSFileProviderItemIdentifier,
        parent: NSFileProviderItemIdentifier,
        filename: String,
        contentType: UTType,
        capabilities: NSFileProviderItemCapabilities
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parent
        self.filename = filename
        self.contentType = contentType
        self.capabilities = capabilities
        super.init()
    }
}
