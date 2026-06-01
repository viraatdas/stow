import FileProvider
import os.log

// Registers (and can tear down) the Stow File Provider domain. Adding the domain
// is what makes "Stow" appear in Finder's sidebar at ~/Library/CloudStorage/Stow.
// The domain persists across launches until explicitly removed.
enum StowDomain {
    static let identifier = NSFileProviderDomainIdentifier(rawValue: "ai.exla.stow.default")
    static let displayName = "Stow"

    private static let log = Logger(subsystem: "ai.exla.stow", category: "domain")

    /// The domain value, reused by the evictor.
    static var domain: NSFileProviderDomain {
        NSFileProviderDomain(identifier: identifier, displayName: displayName)
    }

    /// Register the domain. First removes ALL existing File Provider domains for
    /// this app (clearing any stale/duplicate/wedged state from earlier runs),
    /// then adds exactly one fresh domain. The accumulated duplicate domains were
    /// wedging fileproviderd so it never launched the extension for file ops.
    static func register() {
        stowDiag("removeAllDomains (clean slate)…")
        NSFileProviderManager.removeAllDomains { removeErr in
            if let removeErr {
                stowDiag("removeAllDomains error (continuing): \(removeErr)")
            }
            let domain = NSFileProviderDomain(identifier: identifier, displayName: displayName)
            stowDiag("calling NSFileProviderManager.add(\(displayName))…")
            NSFileProviderManager.add(domain) { error in
                if let error {
                    log.error("Stow domain registration failed: \(error.localizedDescription, privacy: .public)")
                    stowDiag("domain registration FAILED: \(error)")
                } else {
                    log.info("Stow domain registered (\(displayName, privacy: .public))")
                    stowDiag("domain registered OK: \(displayName)")
                }
            }
        }
    }

    /// Remove the domain (used by an eventual `stow uninstall`).
    static func unregister() {
        let domain = NSFileProviderDomain(identifier: identifier, displayName: displayName)
        NSFileProviderManager.remove(domain) { error in
            if let error {
                log.error("Stow domain removal failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
