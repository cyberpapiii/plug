import AppKit
import Foundation
import Observation
import PlugIPC

enum ReconciliationTrigger: Equatable, Sendable {
    case applicationLaunch
    case retry
    case explicitAdoption
}

@MainActor
protocol DaemonServiceManaging: AnyObject {
    func inspect(
        canonical: VerifiedAppInstallation,
        legacyPaths: Set<URL>
    ) async throws -> DaemonServiceSnapshot
    func adoptRecognizedLegacy(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake
    func replaceStaleAppService(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake
    func ensureRunning(expectedVersion: String) async throws -> OperatorHandshake
    func adopt() async throws
}

typealias InstallationDaemonManaging = DaemonServiceManaging

@MainActor
extension DaemonServiceManager: DaemonServiceManaging {}

@MainActor @Observable
final class InstallationCoordinator {
    private static let supportedIPCMin: UInt16 = 3
    private static let supportedIPCMax: UInt16 = 4
    private static let appManagedOwnership = "app_managed"

    private(set) var state: InstallationState

    private let appInspector: any AppInstallationInspecting
    private let legacyMigrator: any LegacyInstallMigrating
    private let clientRepairer: any ClientRepairing
    private let daemonManager: any DaemonServiceManaging
    private let logURL: URL
    private let openURL: (URL) -> Void
    private var inFlight: Task<Void, Never>?

    static let defaultLogURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/Logs/Plug/installation-reconciliation.log")

    init(
        appInspector: any AppInstallationInspecting = AppInstallationInspector(),
        legacyMigrator: any LegacyInstallMigrating = LegacyInstallMigrator(),
        clientRepairer: any ClientRepairing = ClientRepairService(),
        daemonManager: any DaemonServiceManaging = DaemonServiceManager.shared,
        state: InstallationState = .reconcilingUpdate(.inspecting),
        logURL: URL = InstallationCoordinator.defaultLogURL,
        openURL: @escaping (URL) -> Void = { NSWorkspace.shared.open($0) }
    ) {
        self.appInspector = appInspector
        self.legacyMigrator = legacyMigrator
        self.clientRepairer = clientRepairer
        self.daemonManager = daemonManager
        self.state = state
        self.logURL = logURL.standardizedFileURL
        self.openURL = openURL
    }

    func reconcile(trigger: ReconciliationTrigger) async {
        guard shouldStart(trigger: trigger) else { return }

        if let inFlight {
            await inFlight.value
            return
        }

        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            await self.performReconciliation(trigger: trigger)
        }
        inFlight = task
        await task.value
        inFlight = nil
    }

    func adopt() async {
        await reconcile(trigger: .explicitAdoption)
    }

    func retry() async {
        await reconcile(trigger: .retry)
    }

    func openLog() {
        guard case let .blocked(failure) = state,
              let url = failure.logURL
        else { return }
        openURL(url)
    }

    private func shouldStart(trigger: ReconciliationTrigger) -> Bool {
        switch state {
        case .blocked:
            return trigger == .retry || trigger == .explicitAdoption
        case .adoptionRequired:
            return trigger == .explicitAdoption
        case .healthy, .reconcilingUpdate, .repairableDrift:
            return true
        }
    }

    private func performReconciliation(trigger: ReconciliationTrigger) async {
        do {
            let canonical = try await appInspector.inspectCurrentApp()
            var legacy = try await legacyMigrator.inspect(canonical: canonical)

            try rejectUnknownLegacyState(legacy)

            if legacy.formulaInstalled {
                publish(.removingLegacyFormula)
                try await legacyMigrator.removeRecognizedFormula(legacy)
                legacy = legacySnapshot(
                    legacy,
                    formulaInstalled: false
                )
            }

            let shellLink: ShellLinkState
            switch legacy.shellLink {
            case .absent, .repairable:
                publish(.repairingCommand)
                shellLink = try await legacyMigrator.repairShellLink(to: canonical.executableURL)
                legacy = legacySnapshot(legacy, shellLink: shellLink)
            case let .canonical(target):
                shellLink = .canonical(target.standardizedFileURL)
            case let .unrelated(path):
                throw CoordinatorError.unrelatedShellCommand(path)
            }

            var clientsNeedRepair = try await clientRepairer.inspect(
                canonicalExecutable: canonical.executableURL
            )
            if clientsNeedRepair {
                publish(.repairingClients)
                _ = try await clientRepairer.repairAll(
                    canonicalExecutable: canonical.executableURL
                )
                clientsNeedRepair = false
            }

            let daemonSnapshot = try await daemonManager.inspect(
                canonical: canonical,
                legacyPaths: legacy.recognizedPaths
            )
            let handshake = try await reconcileDaemon(
                snapshot: daemonSnapshot,
                canonical: canonical,
                legacy: legacy,
                clientRepairNeeded: clientsNeedRepair,
                trigger: trigger
            )

            let proof = ReconciliationProof(
                appVersion: canonical.appVersion,
                embeddedVersion: canonical.embeddedVersion,
                daemonVersion: handshake.daemonVersion,
                shellTarget: canonical.executableURL.standardizedFileURL,
                appManaged: handshake.ownership == Self.appManagedOwnership
            )
            try requireExactProof(
                proof,
                handshake: handshake,
                canonical: canonical,
                shellLink: shellLink
            )

            if legacy.cargoBinary != nil {
                publish(.cleaningLegacyBinary)
                try await legacyMigrator.removeVerifiedCargoBinary(legacy, proof: proof)
            }

            if case let .reconcilingUpdate(phase) = state, phase != .inspecting {
                publish(.verifying)
            }
            let final = try await inspectFinalState(expected: canonical)
            try requireHealthy(final, expected: canonical)
            state = final
        } catch let error as CoordinatorError {
            publish(error)
        } catch let error as DaemonServiceError {
            publishOperationalFailure(error)
        } catch let error as LegacyInstallError {
            if case let .unrelatedShellCommand(path) = error {
                state = .repairableDrift(
                    InstallationDrift(
                        summary: "A local Plug command conflict needs repair",
                        detail: "An unrelated file or command occupies \(path.path)."
                    )
                )
            } else {
                publishOperationalFailure(error)
            }
        } catch {
            publishOperationalFailure(error)
        }
    }

    private func reconcileDaemon(
        snapshot: DaemonServiceSnapshot,
        canonical: VerifiedAppInstallation,
        legacy: LegacyInstallSnapshot,
        clientRepairNeeded: Bool,
        trigger: ReconciliationTrigger
    ) async throws -> OperatorHandshake {
        switch snapshot.ownership {
        case .recognizedLegacy:
            guard trigger == .explicitAdoption else {
                state = .adoptionRequired(
                    makeSnapshot(
                        app: canonical,
                        legacy: legacy,
                        service: snapshot,
                        clientRepairNeeded: clientRepairNeeded
                    )
                )
                throw CoordinatorError.adoptionRequired
            }
            publish(.replacingDaemon)
            return try await daemonManager.adoptRecognizedLegacy(
                snapshot: snapshot,
                expectedVersion: canonical.appVersion
            )

        case .unmanaged:
            guard trigger == .explicitAdoption else {
                state = .adoptionRequired(
                    makeSnapshot(
                        app: canonical,
                        legacy: legacy,
                        service: snapshot,
                        clientRepairNeeded: clientRepairNeeded
                    )
                )
                throw CoordinatorError.adoptionRequired
            }
            publish(.replacingDaemon)
            try await daemonManager.adopt()
            return try await daemonManager.ensureRunning(expectedVersion: canonical.appVersion)

        case .appManagedStale:
            publish(.replacingDaemon)
            return try await daemonManager.replaceStaleAppService(
                snapshot: snapshot,
                expectedVersion: canonical.appVersion
            )

        case .appManagedCurrent:
            if !isExactService(snapshot, canonical: canonical) {
                publish(.replacingDaemon)
            }
            return try await daemonManager.ensureRunning(expectedVersion: canonical.appVersion)

        case .unknown:
            throw CoordinatorError.unknownOwnership
        }
    }

    private func inspectFinalState(expected canonical: VerifiedAppInstallation) async throws -> InstallationState {
        let finalApp = try await appInspector.inspectCurrentApp()
        guard finalApp == canonical else {
            throw CoordinatorError.finalDisagreement(
                "The verified app changed during reconciliation."
            )
        }

        let finalLegacy = try await legacyMigrator.inspect(canonical: finalApp)
        try rejectUnknownLegacyState(finalLegacy)
        let finalClients = try await clientRepairer.inspect(
            canonicalExecutable: finalApp.executableURL
        )
        let finalService = try await daemonManager.inspect(
            canonical: finalApp,
            legacyPaths: finalLegacy.recognizedPaths
        )
        return .healthy(makeSnapshot(
            app: finalApp,
            legacy: finalLegacy,
            service: finalService,
            clientRepairNeeded: finalClients
        ))
    }

    private func requireHealthy(
        _ final: InstallationState,
        expected canonical: VerifiedAppInstallation
    ) throws {
        guard case let .healthy(snapshot) = final else {
            throw CoordinatorError.finalDisagreement("Final installation state was not healthy.")
        }
        if case .unknown = snapshot.service.ownership {
            throw CoordinatorError.unknownOwnership
        }
        guard snapshot.app == canonical,
              snapshot.daemonVersion == canonical.appVersion,
              !snapshot.clientRepairNeeded,
              isCanonical(snapshot.shellLink, executable: canonical.executableURL),
              snapshot.service.daemonVersion == canonical.appVersion,
              isAppManagedCurrent(snapshot.service.ownership, canonical: canonical),
              snapshot.shadowInstalls.isEmpty
        else {
            throw CoordinatorError.finalDisagreement(
                "Final app, command, client, or daemon evidence disagrees with the verified installation."
            )
        }
    }

    private func makeSnapshot(
        app: VerifiedAppInstallation,
        legacy: LegacyInstallSnapshot,
        service: DaemonServiceSnapshot,
        clientRepairNeeded: Bool
    ) -> InstallationSnapshot {
        InstallationSnapshot(
            app: app,
            shellLink: legacy.shellLink,
            service: service,
            daemonVersion: service.daemonVersion,
            clientRepairNeeded: clientRepairNeeded,
            shadowInstalls: shadowInstalls(legacy: legacy, service: service)
        )
    }

    private func shadowInstalls(
        legacy: LegacyInstallSnapshot,
        service: DaemonServiceSnapshot
    ) -> [ShadowInstall] {
        var shadows: [ShadowInstall] = []
        var knownPaths = Set<URL>()
        if let cargo = legacy.cargoBinary?.standardizedFileURL {
            shadows.append(ShadowInstall(kind: .cargo, url: cargo))
            knownPaths.insert(cargo)
        }
        for path in legacy.recognizedPaths.map(\.standardizedFileURL) {
            guard knownPaths.insert(path).inserted else { continue }
            let kind: ShadowInstall.Kind
            if path.path.hasSuffix("/.cargo/bin/plug") {
                kind = .cargo
            } else if path.path.contains("/opt/plug/bin/plug") {
                kind = .homebrewFormula
            } else {
                kind = .clientLink
            }
            shadows.append(ShadowInstall(kind: kind, url: path))
        }
        if legacy.formulaInstalled,
           !shadows.contains(where: { $0.kind == .homebrewFormula })
        {
            shadows.append(
                ShadowInstall(
                    kind: .homebrewFormula,
                    url: URL(fileURLWithPath: "/opt/homebrew/opt/plug/bin/plug")
                )
            )
        }
        if case let .recognizedLegacy(records) = service.ownership {
            for record in records {
                if let program = record.programURL {
                    shadows.append(ShadowInstall(kind: .launchdJob, url: program.standardizedFileURL))
                }
            }
        }
        return shadows.sorted { $0.url.path < $1.url.path }
    }

    private func legacySnapshot(
        _ snapshot: LegacyInstallSnapshot,
        formulaInstalled: Bool? = nil,
        shellLink: ShellLinkState? = nil
    ) -> LegacyInstallSnapshot {
        LegacyInstallSnapshot(
            formulaInstalled: formulaInstalled ?? snapshot.formulaInstalled,
            cargoBinary: snapshot.cargoBinary,
            shellLink: shellLink ?? snapshot.shellLink,
            recognizedPaths: snapshot.recognizedPaths,
            unknownPaths: snapshot.unknownPaths
        )
    }

    private func rejectUnknownLegacyState(_ snapshot: LegacyInstallSnapshot) throws {
        guard snapshot.unknownPaths.isEmpty else {
            let paths = snapshot.unknownPaths.map(\.path).sorted().joined(separator: ", ")
            throw CoordinatorError.unknownLocalState(paths)
        }
        if case let .unrelated(path) = snapshot.shellLink {
            throw CoordinatorError.unrelatedShellCommand(path)
        }
    }

    private func requireExactProof(
        _ proof: ReconciliationProof,
        handshake: OperatorHandshake,
        canonical: VerifiedAppInstallation,
        shellLink: ShellLinkState
    ) throws {
        guard proof.appManaged,
              handshake.ownership == Self.appManagedOwnership,
              isCompatible(handshake),
              proof.appVersion == canonical.appVersion,
              proof.embeddedVersion == canonical.embeddedVersion,
              proof.daemonVersion == canonical.appVersion,
              isCanonical(shellLink, executable: canonical.executableURL)
        else {
            throw CoordinatorError.proofDisagreement
        }
    }

    private func isCompatible(_ handshake: OperatorHandshake) -> Bool {
        guard handshake.ipcMin <= handshake.ipcMax else { return false }
        return handshake.ipcMin <= Self.supportedIPCMax
            && handshake.ipcMax >= Self.supportedIPCMin
    }

    private func isExactService(
        _ snapshot: DaemonServiceSnapshot,
        canonical: VerifiedAppInstallation
    ) -> Bool {
        snapshot.daemonVersion == canonical.appVersion
            && snapshot.daemonExecutable.map { samePath($0, canonical.executableURL) } == true
            && isAppManagedCurrent(snapshot.ownership, canonical: canonical)
    }

    private func isAppManagedCurrent(
        _ ownership: DaemonOwnershipState,
        canonical: VerifiedAppInstallation
    ) -> Bool {
        guard case let .appManagedCurrent(record) = ownership,
              let executable = record.programURL
        else { return false }
        return samePath(executable, canonical.executableURL)
            && record.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
            && record.parentBundleVersion == canonical.buildVersion
    }

    private func isCanonical(_ shellLink: ShellLinkState, executable: URL) -> Bool {
        guard case let .canonical(target) = shellLink else { return false }
        return samePath(target, executable)
    }

    private func samePath(_ lhs: URL, _ rhs: URL) -> Bool {
        lhs.standardizedFileURL.resolvingSymlinksInPath().standardizedFileURL
            == rhs.standardizedFileURL.resolvingSymlinksInPath().standardizedFileURL
    }

    private func publish(_ phase: ReconciliationPhase) {
        state = .reconcilingUpdate(phase)
    }

    private func publish(_ error: CoordinatorError) {
        switch error {
        case let .unrelatedShellCommand(path):
            state = .repairableDrift(
                InstallationDrift(
                    summary: "A local Plug command conflict needs repair",
                    detail: "An unrelated file or command occupies \(path.path)."
                )
            )
        case let .unknownLocalState(paths):
            state = .repairableDrift(
                InstallationDrift(
                    summary: "Unknown local Plug state needs review",
                    detail: "Unknown installation paths were found: \(paths)."
                )
            )
        case .adoptionRequired:
            if case .adoptionRequired = state { return }
            state = .blocked(
                InstallationFailure(
                    summary: "Plug adoption is required",
                    detail: "The existing daemon requires explicit adoption.",
                    logURL: logURL
                )
            )
        case .unknownOwnership:
            state = .blocked(
                InstallationFailure(
                    summary: "Plug daemon ownership is unknown",
                    detail: "Launchd evidence did not prove ownership of the daemon.",
                    logURL: logURL
                )
            )
        case .proofDisagreement, .finalDisagreement:
            state = .repairableDrift(
                InstallationDrift(
                    summary: "Plug installation did not converge",
                    detail: error.detail
                )
            )
        }
    }

    private func publishOperationalFailure(_ error: Error) {
        state = .blocked(
            InstallationFailure(
                summary: "Plug installation reconciliation failed",
                detail: String(describing: error),
                logURL: logURL
            )
        )
    }
}

private enum CoordinatorError: Error {
    case adoptionRequired
    case unrelatedShellCommand(URL)
    case unknownLocalState(String)
    case unknownOwnership
    case proofDisagreement
    case finalDisagreement(String)

    var detail: String {
        switch self {
        case .adoptionRequired:
            return "Explicit adoption is required before the existing daemon can be replaced."
        case let .unrelatedShellCommand(path):
            return "An unrelated file or command occupies \(path.path)."
        case let .unknownLocalState(paths):
            return "Unknown installation paths were found: \(paths)."
        case .unknownOwnership:
            return "Launchd evidence did not prove ownership of the daemon."
        case .proofDisagreement:
            return "The app, command, and daemon proof did not agree."
        case let .finalDisagreement(detail):
            return detail
        }
    }
}
