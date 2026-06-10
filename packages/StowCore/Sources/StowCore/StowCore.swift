import Foundation
import CStowCore

/// Swift wrapper over the Rust core's C ABI (`CStowCore` from StowCore.xcframework).
/// The engine functions return JSON strings which we decode here.
public enum StowCoreLib {
    /// Version string baked into the Rust core.
    public static func version() -> String {
        guard let cstr = stow_core_version() else { return "unknown" }
        defer { stow_string_free(cstr) }
        return String(cString: cstr)
    }

    static let appGroup = "3C4383262W.ai.exla.stow"

    /// Point the Rust core at the shared App Group container so the sandboxed
    /// extension, the agent, and the CLI all read/write the same config + DBs.
    /// MUST be called before any other FFI use. Idempotent.
    public static func bootstrap() {
        // Never let the AWS SDK probe the EC2 instance-metadata endpoint. On a
        // laptop (and especially inside the File Provider sandbox) that endpoint
        // is unreachable and the SDK blocks on it for a long time — which made
        // createItem/fetchContents hang and file copies into the Stow folder
        // time out. We only ever use explicit creds from the shared config.
        setenv("AWS_EC2_METADATA_DISABLED", "true", 1)
        let path: String
        if let url = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroup) {
            // Sandboxed members (extension, agent) — OS-resolved container.
            path = url.path
        } else {
            // Non-sandboxed CLI: reach the same dir by absolute path under the
            // real home. (homeDirectoryForCurrentUser is the real home here.)
            path = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Group Containers/\(appGroup)").path
        }
        try? FileManager.default.createDirectory(
            atPath: path, withIntermediateDirectories: true)
        setenv("STOW_GROUP_DIR", path, 1)
    }

    /// Absolute path of the shared App Group container, if available.
    public static var groupContainerPath: String? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroup)?.path
    }
}

/// An error surfaced from the Rust engine (`{"error":...,"code":N}` JSON).
public struct StowError: Error, CustomStringConvertible {
    public let message: String
    public let code: Int
    public var description: String { message }
}

/// Calls into the Rust engine and returns decoded JSON, throwing `StowError`
/// when the engine reports a failure.
public enum StowEngine {
    /// Take ownership of a C string the core allocated, convert + free it.
    private static func take(_ ptr: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        guard let ptr else { throw StowError(message: "core returned null", code: -1) }
        defer { stow_string_free(ptr) }
        let json = String(cString: ptr)
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw StowError(message: "invalid response from core: \(json)", code: -1)
        }
        if let err = obj["error"] as? String {
            throw StowError(message: err, code: (obj["code"] as? Int) ?? 1)
        }
        return obj
    }

    /// `stow init` — auto-provision S3 + persist config.
    public static func initialize(region: String?) throws -> [String: Any] {
        if let region {
            return try region.withCString { try take(stow_engine_init($0)) }
        } else {
            return try take(stow_engine_init(nil))
        }
    }

    /// `stow add/offload <path>`.
    public static func offload(_ path: String) throws -> [String: Any] {
        try path.withCString { try take(stow_engine_offload($0)) }
    }

    /// `stow restore <path>`.
    public static func restore(_ path: String) throws -> [String: Any] {
        try path.withCString { try take(stow_engine_restore($0)) }
    }

    /// `stow status`.
    public static func status() throws -> [String: Any] {
        try take(stow_engine_status())
    }

    /// `stow scan` — dry run; list auto-offload candidates.
    public static func scan() throws -> [String: Any] {
        try take(stow_engine_scan())
    }

    /// `stow auto` — apply the policy and offload candidates.
    public static func auto() throws -> [String: Any] {
        try take(stow_engine_auto())
    }

    /// `stow clean` — reclaim regenerable tool/package caches idle ≥ minIdleDays.
    /// `apply: false` is a dry run. Returns a CleanReport object.
    public static func cleanCaches(minIdleDays: UInt64, apply: Bool) throws -> [String: Any] {
        try take(stow_engine_clean_caches(minIdleDays, apply))
    }

    /// Full persisted config (bucket/region/policy).
    public static func getConfig() throws -> [String: Any] {
        try take(stow_engine_get_config())
    }

    /// `stow migrate` — convert legacy stub offloads to transparent symlinks.
    public static func migrate() throws -> [String: Any] {
        try take(stow_engine_migrate())
    }

    /// `stow share <path>` — publish a permanent public link (folders zipped).
    public static func share(_ path: String) throws -> [String: Any] {
        try path.withCString { try take(stow_engine_share($0)) }
    }

    /// `stow unshare <token>` — revoke a link; returns the removed share row.
    @discardableResult
    public static func unshare(_ token: String) throws -> [String: Any] {
        try token.withCString { try take(stow_engine_unshare($0)) }
    }

    /// `stow shares` — active links, newest first ({"shares":[...]}).
    public static func listShares() throws -> [String: Any] {
        try take(stow_engine_list_shares())
    }

    /// Replace the policy block (JSON object). Returns updated config.
    @discardableResult
    public static func setPolicy(_ json: String) throws -> [String: Any] {
        try json.withCString { try take(stow_engine_set_policy($0)) }
    }
}

/// One item in the File Provider domain, decoded from the Rust core JSON.
public struct StowFPItem {
    public let itemID: String
    public let parentID: String
    public let filename: String
    public let isFolder: Bool
    public let size: Int64
    public let contentType: String
    public let hash: String?
    public let version: Int64
    public let modifiedAt: Int64
    public let lastAccess: Int64
    public let dataless: Bool

    init?(_ d: [String: Any]) {
        guard let itemID = d["item_id"] as? String,
              let parentID = d["parent_id"] as? String,
              let filename = d["filename"] as? String else { return nil }
        self.itemID = itemID
        self.parentID = parentID
        self.filename = filename
        self.isFolder = d["is_folder"] as? Bool ?? false
        self.size = (d["size"] as? NSNumber)?.int64Value ?? 0
        self.contentType = d["content_type"] as? String ?? "public.data"
        self.hash = d["hash"] as? String
        self.version = (d["version"] as? NSNumber)?.int64Value ?? 1
        self.modifiedAt = (d["modified_at"] as? NSNumber)?.int64Value ?? 0
        self.lastAccess = (d["last_access"] as? NSNumber)?.int64Value ?? 0
        self.dataless = d["dataless"] as? Bool ?? false
    }
}

/// Swift wrapper over the File Provider FFI. Used by the extension.
public enum StowProvider {
    private static func object(_ ptr: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        guard let ptr else { throw StowError(message: "core returned null", code: -1) }
        defer { stow_string_free(ptr) }
        let json = String(cString: ptr)
        guard let data = json.data(using: .utf8) else {
            throw StowError(message: "invalid response", code: -1)
        }
        let parsed = try? JSONSerialization.jsonObject(with: data)
        if let obj = parsed as? [String: Any] {
            if let err = obj["error"] as? String {
                throw StowError(message: err, code: (obj["code"] as? Int) ?? 1)
            }
            return obj
        }
        throw StowError(message: "expected object: \(json)", code: -1)
    }

    private static func array(_ ptr: UnsafeMutablePointer<CChar>?) throws -> [[String: Any]] {
        guard let ptr else { throw StowError(message: "core returned null", code: -1) }
        defer { stow_string_free(ptr) }
        let json = String(cString: ptr)
        guard let data = json.data(using: .utf8) else {
            throw StowError(message: "invalid response", code: -1)
        }
        let parsed = try? JSONSerialization.jsonObject(with: data)
        if let arr = parsed as? [[String: Any]] { return arr }
        if let obj = parsed as? [String: Any], let err = obj["error"] as? String {
            throw StowError(message: err, code: (obj["code"] as? Int) ?? 1)
        }
        throw StowError(message: "expected array: \(json)", code: -1)
    }

    /// Children of a container.
    public static func enumerate(parentID: String) throws -> [StowFPItem] {
        try parentID.withCString { try array(stow_fp_enumerate($0)).compactMap(StowFPItem.init) }
    }

    /// A single item by id.
    public static func item(id: String) throws -> StowFPItem {
        let d = try id.withCString { try object(stow_fp_item($0)) }
        guard let it = StowFPItem(d) else { throw StowError(message: "bad item", code: -1) }
        return it
    }

    /// Create a file (uploads `tempPath` to S3) or folder.
    @discardableResult
    public static func create(parentID: String, filename: String,
                              isFolder: Bool, tempPath: String?) throws -> StowFPItem {
        let d = try parentID.withCString { p in
            try filename.withCString { f in
                if let tp = tempPath {
                    return try tp.withCString { t in try object(stow_fp_create(p, f, isFolder, t)) }
                } else {
                    return try object(stow_fp_create(p, f, isFolder, nil))
                }
            }
        }
        guard let it = StowFPItem(d) else { throw StowError(message: "bad item", code: -1) }
        return it
    }

    /// Replace an item's contents from `tempPath`.
    @discardableResult
    public static func modify(id: String, tempPath: String) throws -> StowFPItem {
        let d = try id.withCString { i in
            try tempPath.withCString { t in try object(stow_fp_modify(i, t)) }
        }
        guard let it = StowFPItem(d) else { throw StowError(message: "bad item", code: -1) }
        return it
    }

    /// Download an item's bytes from S3 to `outPath` (rehydrate on open).
    @discardableResult
    public static func fetch(id: String, outPath: String) throws -> StowFPItem {
        let d = try id.withCString { i in
            try outPath.withCString { o in try object(stow_fp_fetch(i, o)) }
        }
        guard let it = StowFPItem(d) else { throw StowError(message: "bad item", code: -1) }
        return it
    }

    /// Delete an item (metadata).
    public static func delete(id: String) throws {
        _ = try id.withCString { try object(stow_fp_delete($0)) }
    }

    /// Sync the shared DB's dataless flag after an eviction/materialization, so
    /// `stow status` can list offloaded folder files accurately.
    public static func setDataless(id: String, _ dataless: Bool) throws {
        _ = try id.withCString { try object(stow_fp_set_dataless($0, dataless)) }
    }

    /// Fingerprint of the provider DB — the sync anchor. Changes whenever any
    /// row is inserted, modified, or deleted.
    public static func anchor() throws -> String {
        let d = try object(stow_fp_anchor())
        guard let a = d["anchor"] as? String else {
            throw StowError(message: "bad anchor", code: -1)
        }
        return a
    }
}
