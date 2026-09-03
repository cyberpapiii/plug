import Foundation

enum InstallationState: Equatable {
    case healthy(InstallationSnapshot)
    case adoptionRequired(InstallationSnapshot)
    case reconcilingUpdate(ReconciliationPhase)
    case repairableDrift(InstallationDrift)
    case blocked(InstallationFailure)
}

enum ReconciliationPhase: Equatable {
    case inspecting
    case removingLegacyFormula
    case repairingCommand
    case repairingClients
    case replacingDaemon
    case verifying
    case cleaningLegacyBinary
    /// A command timed out, which right after login usually means the Mac is
    /// still busy rather than broken. The coordinator tries again by itself.
    case waitingToRetry
}

enum ShellLinkState: Equatable, Sendable {
    case absent
    case canonical(URL)
    case repairable(URL?)
    case unrelated(URL)
}

struct ShadowInstall: Equatable, Sendable {
    enum Kind: String, Sendable {
        case cargo
        case homebrewFormula
        case clientLink
        case launchdJob
    }

    let kind: Kind
    let url: URL
}

struct InstallationSnapshot: Equatable, Sendable {
    let app: VerifiedAppInstallation
    let shellLink: ShellLinkState
    let service: DaemonServiceSnapshot
    let daemonVersion: String?
    let clientRepairNeeded: Bool
    let shadowInstalls: [ShadowInstall]
}

struct InstallationDrift: Equatable, Sendable {
    let summary: String
    let detail: String
}

struct InstallationFailure: Equatable, Sendable {
    let summary: String
    let detail: String
    let logURL: URL?
}

// Defined here because installation state stores these values. Inspectors added
// by the next reconciliation task populate them.
struct VerifiedAppInstallation: Equatable, Sendable {
    let bundleURL: URL
    let executableURL: URL
    let appVersion: String
    let buildVersion: String
    let embeddedVersion: String
    let teamID: String
}

struct DaemonServiceSnapshot: Equatable, Sendable {
    let ownership: DaemonOwnershipState
    let daemonVersion: String?
    let daemonExecutable: URL?
}

enum DaemonOwnershipState: Equatable, Sendable {
    case appManagedCurrent(LaunchdJobRecord)
    case appManagedStale(LaunchdJobRecord)
    case recognizedLegacy([LaunchdJobRecord])
    case unmanaged
    case unknown([LaunchdJobRecord])

    var programURLs: Set<URL> {
        switch self {
        case let .recognizedLegacy(records), let .unknown(records):
            Set(records.compactMap { $0.programURL?.standardizedFileURL })
        case let .appManagedCurrent(record), let .appManagedStale(record):
            Set([record.programURL].compactMap { $0?.standardizedFileURL })
        case .unmanaged:
            []
        }
    }
}

struct LaunchdJobRecord: Equatable, Sendable {
    let label: String
    let programURL: URL?
    let parentBundleIdentifier: String?
    let parentBundleVersion: String?
    let loaded: Bool
    let programIdentifier: String?
    let arguments: [String]

    init(
        label: String,
        programURL: URL?,
        parentBundleIdentifier: String?,
        parentBundleVersion: String?,
        loaded: Bool,
        programIdentifier: String? = nil,
        arguments: [String] = []
    ) {
        self.label = label
        self.programURL = programURL
        self.parentBundleIdentifier = parentBundleIdentifier
        self.parentBundleVersion = parentBundleVersion
        self.loaded = loaded
        self.programIdentifier = programIdentifier
        self.arguments = arguments
    }
}

/// Keep in lockstep with `is_recognized_legacy_program` in `plug/src/service.rs`.
/// `testdata/legacy_plug_programs.json` pins the table itself as well as the
/// cases, so a shape added to one language without the other fails a test on
/// both sides.
enum LegacyPlugProgram {
    static let exactPaths = [
        "/opt/homebrew/bin/plug",
        "/usr/local/bin/plug",
        "/opt/homebrew/opt/plug/bin/plug",
        "/usr/local/opt/plug/bin/plug",
    ]
    static let homeRelativePaths = [".cargo/bin/plug", ".local/bin/plug"]
    static let cellarRoots = ["/opt/homebrew/Cellar/plug", "/usr/local/Cellar/plug"]

    static func isRecognized(_ url: URL) -> Bool {
        let path = url.standardizedFileURL.path
        if isHomebrewInstall(path) {
            return true
        }
        let home = FileManager.default.homeDirectoryForCurrentUser.standardizedFileURL.path
        return homeRelativePaths.contains { path == "\(home)/\($0)" }
    }

    static func isHomebrew(_ url: URL) -> Bool {
        isHomebrewInstall(url.standardizedFileURL.path)
    }

    private static func isHomebrewInstall(_ path: String) -> Bool {
        exactPaths.contains(path) || cellarRoots.contains { isCellarBinary(path, root: $0) }
    }

    /// A Cellar binary is a shape, not a path: `<root>/<version>/bin/plug`.
    private static func isCellarBinary(_ path: String, root: String) -> Bool {
        guard path.hasPrefix(root + "/") else { return false }
        let parts = path.dropFirst(root.count + 1).split(separator: "/")
        return parts.count == 3 && parts[1] == "bin" && parts[2] == "plug"
    }
}
