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
}

struct LaunchdJobRecord: Equatable, Sendable {
    let label: String
    let programURL: URL?
    let parentBundleIdentifier: String?
    let parentBundleVersion: String?
    let loaded: Bool
}
