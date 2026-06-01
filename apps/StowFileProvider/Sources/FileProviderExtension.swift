import FileProvider
import os.log

/// Append a timestamped trace line to a file the sandboxed extension can write:
/// the App Group container. This is how we see what the extension does without
/// access to fileproviderd's privileged logs.
func fpTrace(_ s: String) {
    let dir = FileManager.default
        .containerURL(forSecurityApplicationGroupIdentifier: "group.ai.exla.stow")
    guard let dir else { return }
    let path = dir.appendingPathComponent("ext-trace.log").path
    let line = "[\(Date().timeIntervalSince1970)] \(s)\n"
    if let h = FileHandle(forWritingAtPath: path) {
        h.seekToEndOfFile(); h.write(Data(line.utf8)); try? h.close()
    } else {
        try? line.write(toFile: path, atomically: true, encoding: .utf8)
    }
}

/// The replicated File Provider extension — the OS-driven hot path. `fileproviderd`
/// instantiates this per domain. It delegates all storage to the Rust core
/// (`StowProvider`), which talks to S3 + the shared SQLite index.
///
/// - `enumerator`/`item` render the Stow folder from the index.
/// - `createItem`/`modifyItem` upload new/changed files to S3.
/// - `fetchContents` downloads bytes from S3 on demand (rehydrate on open).
final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain
    private let log = Logger(subsystem: "ai.exla.stow", category: "fileprovider")
    private let work = DispatchQueue(label: "ai.exla.stow.fp", qos: .userInitiated, attributes: .concurrent)

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
        // Point the Rust core at the shared App Group container (we're sandboxed).
        StowCoreLib.bootstrap()
        fpTrace("init: domain=\(domain.identifier.rawValue) core=\(StowCoreLib.version())")
        log.info("StowFileProvider init; core v\(StowCoreLib.version(), privacy: .public)")
    }

    func invalidate() {}

    // MARK: - Metadata

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        if identifier == .rootContainer {
            completionHandler(StowItem.root, nil)
            return Progress()
        }
        work.async {
            do {
                let it = try StowProvider.item(id: identifier.rawValue)
                completionHandler(StowItem(it), nil)
            } catch {
                completionHandler(nil, NSFileProviderError(.noSuchItem))
            }
        }
        return Progress()
    }

    // MARK: - Rehydrate on open

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 100)
        fpTrace("fetchContents START id=\(itemIdentifier.rawValue)")
        work.async {
            do {
                // Download to a unique temp file, then hand the URL to the OS.
                let tmp = FileManager.default.temporaryDirectory
                    .appendingPathComponent(UUID().uuidString)
                let it = try StowProvider.fetch(id: itemIdentifier.rawValue, outPath: tmp.path)
                fpTrace("fetchContents got bytes; completing")
                progress.completedUnitCount = 100
                completionHandler(tmp, StowItem(it), nil)
            } catch {
                fpTrace("fetchContents ERROR \(error)")
                self.log.error("fetchContents failed: \(error.localizedDescription, privacy: .public)")
                completionHandler(nil, nil, NSFileProviderError(.serverUnreachable))
            }
        }
        return progress
    }

    // MARK: - Create (upload)

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let parent = itemTemplate.parentItemIdentifier == .rootContainer
            ? NSFileProviderItemIdentifier.rootContainer.rawValue
            : itemTemplate.parentItemIdentifier.rawValue
        let name = itemTemplate.filename
        let isFolder = (itemTemplate.contentType == .folder)
        fpTrace("createItem START name=\(name) folder=\(isFolder) hasContents=\(url != nil)")
        work.async {
            do {
                fpTrace("createItem calling Rust create…")
                let it = try StowProvider.create(parentID: parent, filename: name,
                                                 isFolder: isFolder, tempPath: url?.path)
                fpTrace("createItem Rust returned id=\(it.itemID); calling completion")
                completionHandler(StowItem(it), [], false, nil)
                fpTrace("createItem DONE")
            } catch {
                fpTrace("createItem ERROR \(error)")
                self.log.error("createItem failed: \(error.localizedDescription, privacy: .public)")
                completionHandler(nil, [], false, error)
            }
        }
        return Progress()
    }

    // MARK: - Modify

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion version: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let id = item.itemIdentifier.rawValue
        work.async {
            do {
                if changedFields.contains(.contents), let url = newContents {
                    let it = try StowProvider.modify(id: id, tempPath: url.path)
                    completionHandler(StowItem(it), [], false, nil)
                } else {
                    // Metadata-only change we don't track yet: echo current state.
                    let it = try StowProvider.item(id: id)
                    completionHandler(StowItem(it), [], false, nil)
                }
            } catch {
                completionHandler(nil, [], false, error)
            }
        }
        return Progress()
    }

    // MARK: - Delete

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        work.async {
            do {
                try StowProvider.delete(id: identifier.rawValue)
                completionHandler(nil)
            } catch {
                completionHandler(error)
            }
        }
        return Progress()
    }

    // MARK: - Enumeration

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        FileProviderEnumerator(container: containerItemIdentifier)
    }
}
