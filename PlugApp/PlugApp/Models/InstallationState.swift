import Foundation
import PlugIPC

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

struct InstallationSnapshot: Equatable, Sendable {
    let app: VerifiedAppInstallation
    let shellLink: ShellLinkState
    let service: DaemonServiceSnapshot
    let daemonVersion: String?
    let clientRepairNeeded: Bool
    /// Other copies of Plug still on this Mac.
    let shadowInstalls: [URL]
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

/// This app's bundle, signed by Plug's team, and the daemon binary inside it,
/// which the inspector found to be the app's own version.
struct VerifiedAppInstallation: Equatable, Sendable {
    let bundleURL: URL
    let executableURL: URL
    let appVersion: String
    let buildVersion: String
}

// MARK: - Whether the daemon is this app's own

extension VerifiedAppInstallation {
    /// A launchd job this app registered, at any build: it runs this bundle's
    /// binary, and SMAppService recorded this bundle as its parent.
    func registered(_ record: LaunchdJobRecord) -> Bool {
        record.programURL.map(isExecutable) == true
            && record.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
    }

    /// Registered by this very build of the bundle.
    func registeredCurrent(_ record: LaunchdJobRecord) -> Bool {
        registered(record) && record.parentBundleVersion == buildVersion
    }

    /// A daemon that answers as this build: this version, from this binary.
    func answers(version: String?, executable: URL?) -> Bool {
        version == appVersion && executable.map(isExecutable) == true
    }

    /// The one test of "the daemon is this app's own current build": launchd
    /// runs it for this build of the bundle, and the daemon answering says it
    /// is this version, running from this bundle's binary.
    func isOwnCurrentDaemon(_ record: LaunchdJobRecord, version: String?, executable: URL?) -> Bool {
        registeredCurrent(record) && answers(version: version, executable: executable)
    }

    func isOwnCurrentDaemon(_ service: DaemonServiceSnapshot) -> Bool {
        guard case let .appManagedCurrent(record) = service.ownership else { return false }
        return isOwnCurrentDaemon(record, version: service.daemonVersion, executable: service.daemonExecutable)
    }

    /// The same file as this bundle's binary, by whatever route.
    func isExecutable(_ url: URL) -> Bool {
        url.resolvedStandardized == executableURL.resolvedStandardized
    }
}

/// What a pass that changed the daemon ends on: the daemon answering is this
/// app's own build, it says launchd runs it for this app, and the shell
/// command runs the same binary. Only `init?` makes one, so holding one is
/// the proof, and removing a leftover copy of Plug asks for one.
struct OwnDaemonProof: Sendable {
    let app: VerifiedAppInstallation

    init?(app: VerifiedAppInstallation, handshake: OperatorHandshake, shellLink: ShellLinkState) {
        guard handshake.ownership == .appManaged,
              app.answers(version: handshake.daemonVersion, executable: handshake.daemonExecutable),
              case let .canonical(target) = shellLink, app.isExecutable(target)
        else { return nil }
        self.app = app
    }
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
    let programIdentifier: String?
    let arguments: [String]
    /// Who registered the job. `com.apple.xpc.ServiceManagement` for one
    /// SMAppService registered on an app's behalf.
    let managedBy: String?

    init(
        label: String,
        programURL: URL?,
        parentBundleIdentifier: String?,
        parentBundleVersion: String?,
        programIdentifier: String? = nil,
        arguments: [String] = [],
        managedBy: String? = nil
    ) {
        self.label = label
        self.programURL = programURL
        self.parentBundleIdentifier = parentBundleIdentifier
        self.parentBundleVersion = parentBundleVersion
        self.programIdentifier = programIdentifier
        self.arguments = arguments
        self.managedBy = managedBy
    }
}

/// Keep in lockstep with `is_recognized_legacy_program` in `plug/src/service.rs`.
/// `testdata/legacy_plug_programs.json` pins the table itself as well as the
/// cases, so a shape added to one language without the other fails a test on
/// both sides.
enum LegacyPlugProgram {
    /// Where `cargo install` put it.
    static let cargoBinary = ".cargo/bin/plug"
    /// The command on the shell's path. Plug now links it to the app's binary.
    static let shellLink = ".local/bin/plug"
    /// The Homebrew formula's own links, one per Homebrew prefix.
    static let formulaLinks = ["/opt/homebrew/opt/plug/bin/plug", "/usr/local/opt/plug/bin/plug"]
    /// The Homebrew formula, by name.
    static let formula = "cyberpapiii/tap/plug"

    static let exactPaths = ["/opt/homebrew/bin/plug", "/usr/local/bin/plug"] + formulaLinks
    static let homeRelativePaths = [cargoBinary, shellLink]
    static let cellarRoots = ["/opt/homebrew/Cellar/plug", "/usr/local/Cellar/plug"]

    /// Every fixed place a copy of Plug was ever installed, for a home
    /// directory. Cellar copies are recognized by shape instead.
    static func installPaths(home: URL) -> Set<URL> {
        Set(exactPaths.map { URL(fileURLWithPath: $0) } + homeRelativePaths.map { home.appending(path: $0) })
    }

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
