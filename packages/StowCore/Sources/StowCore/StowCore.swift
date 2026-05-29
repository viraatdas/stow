import Foundation
import CStowCore

/// Idiomatic Swift wrapper over the Rust core's C ABI (`libstow_core`, exposed as
/// the `CStowCore` clang module from StowCore.xcframework).
///
/// In M0 this proves the static library links and is callable from both the app
/// and the File Provider extension. The data-plane methods (fetch/upload) get
/// real bodies in M1.
public enum StowCoreLib {
    /// Version string baked into the Rust core. The trivial cross-FFI smoke test.
    public static func version() -> String {
        guard let cstr = stow_core_version() else { return "unknown" }
        defer { stow_string_free(cstr) }
        return String(cString: cstr)
    }
}

/// Status codes mirrored from the Rust `StowStatus` enum, for ergonomic Swift use.
public enum StowStatusCode: Int32 {
    case ok = 0
    case unknown = 1
    case panic = 2
    case invalidArg = 3
    case invalidConfig = 4
    case unimplemented = 5
    case notFound = 6
    case io = 7
    case network = 8
    case cancelled = 9
    case integrity = 10
}

/// An error surfaced from the Rust core (nonzero status + message via `err_out`).
public struct StowCoreError: Error, CustomStringConvertible {
    public let status: StowStatusCode
    public let message: String
    public var description: String { "StowCoreError(\(status)): \(message)" }
}

/// A live handle to the Rust core. Owns the underlying `StowCore*` and frees it
/// on deinit. Constructed from a JSON config string (bucket/region/prefix/…).
public final class StowCore {
    private let handle: OpaquePointer

    /// Create a core from a JSON config. Throws `StowCoreError` on bad config.
    public init(configJSON: String) throws {
        var errPtr: UnsafeMutablePointer<CChar>? = nil
        guard let h = stow_core_new(configJSON, &errPtr) else {
            let msg = errPtr.map { p -> String in
                defer { stow_string_free(p) }
                return String(cString: p)
            } ?? "unknown error"
            throw StowCoreError(status: .invalidConfig, message: msg)
        }
        self.handle = h
    }

    deinit {
        stow_core_free(handle)
    }

    /// Underlying handle, for the data-plane calls wired up in M1.
    var raw: OpaquePointer { handle }
}
