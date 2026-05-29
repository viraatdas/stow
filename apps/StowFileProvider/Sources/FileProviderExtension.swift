import FileProvider
import os.log

/// The replicated File Provider extension — the OS-driven hot path. `fileproviderd`
/// instantiates this per domain and calls `fetchContents` to rehydrate dataless
/// files on access.
///
/// M0: skeleton that compiles, links the Rust core, and satisfies the
/// `NSFileProviderReplicatedExtension` contract with not-implemented stubs. The
/// real enumeration / upload / rehydration logic lands in M1.
final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain
    private let log = Logger(subsystem: "ai.exla.stow", category: "fileprovider")

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
        // Prove the Rust static library also links into the extension process.
        log.info("StowFileProvider init for domain \(domain.displayName, privacy: .public); core v\(StowCoreLib.version(), privacy: .public)")
    }

    func invalidate() {
        // Tear down per-domain resources (S3 client, DB handle) in M1.
    }

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, NSFileProviderError(.noSuchItem))
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        // M1: look up the S3 key in the index, download via stow_fetch_object,
        // return the materialized temp URL here.
        completionHandler(nil, nil, StowError.notImplemented)
        return Progress()
    }

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, [], false, StowError.notImplemented)
        return Progress()
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion version: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, [], false, StowError.notImplemented)
        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        completionHandler(StowError.notImplemented)
        return Progress()
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        FileProviderEnumerator(enumeratedItemIdentifier: containerItemIdentifier)
    }
}

/// Placeholder error until typed errors land in M1.
enum StowError: Error {
    case notImplemented
}
