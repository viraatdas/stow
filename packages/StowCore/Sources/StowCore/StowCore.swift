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

    /// Full persisted config (bucket/region/policy).
    public static func getConfig() throws -> [String: Any] {
        try take(stow_engine_get_config())
    }

    /// Replace the policy block (JSON object). Returns updated config.
    @discardableResult
    public static func setPolicy(_ json: String) throws -> [String: Any] {
        try json.withCString { try take(stow_engine_set_policy($0)) }
    }
}
