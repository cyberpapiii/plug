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
    var appServiceEnabled: Bool { get }
    func inspect(
        canonical: VerifiedAppInstallation,
        legacyPaths: Set<URL>
    ) async throws -> DaemonServiceSnapshot
    func bootOutRecognizedLegacy(_ snapshot: DaemonServiceSnapshot) async throws
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
    private static let supportedIPCMax: UInt16 = 6
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

            var inspectTimeLegacyPaths = Set<URL>()
            var leftoverAdoptSnapshot: DaemonServiceSnapshot?
            if legacy.formulaInstalled {
                let preUninstallDaemon = try await daemonManager.inspect(
                    canonical: canonical,
                    legacyPaths: legacy.recognizedPaths
                )
                inspectTimeLegacyPaths = programURLs(from: preUninstallDaemon)
                if case .recognizedLegacy = preUninstallDaemon.ownership {
                    leftoverAdoptSnapshot = preUninstallDaemon
                }
                try await bootOutHomebrewLegacyIfNeeded(preUninstallDaemon)
                publish(.removingLegacyFormula)
                try await legacyMigrator.removeRecognizedFormula(legacy)
                legacy = legacySnapshot(
                    legacy,
                    formulaInstalled: false,
                    recognizedPaths: legacy.recognizedPaths.union(inspectTimeLegacyPaths)
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

            let liveDaemon = try await daemonManager.inspect(
                canonical: canonical,
                legacyPaths: legacy.recognizedPaths.union(inspectTimeLegacyPaths)
            )
            let daemonSnapshot = adoptionSnapshot(
                live: liveDaemon,
                leftover: leftoverAdoptSnapshot
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
                daemonExecutable: handshake.daemonExecutable?.standardizedFileURL,
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
            guard trigger == .explicitAdoption || daemonManager.appServiceEnabled else {
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
            guard trigger == .explicitAdoption || daemonManager.appServiceEnabled else {
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
        let disagreements = finalDisagreements(snapshot, expected: canonical)
        guard disagreements.isEmpty else {
            throw CoordinatorError.finalDisagreement(
                "Final evidence disagrees with the verified installation: "
                    + disagreements.joined(separator: " ")
            )
        }
    }

    /// Names every final check that failed so the drift banner can say what
    /// actually disagreed instead of listing every possible cause.
    private func finalDisagreements(
        _ snapshot: InstallationSnapshot,
        expected canonical: VerifiedAppInstallation
    ) -> [String] {
        var failures: [String] = []
        if snapshot.app != canonical {
            failures.append("The app bundle changed during reconciliation.")
        }
        if snapshot.daemonVersion != canonical.appVersion {
            failures.append(
                "The daemon reports version \(snapshot.daemonVersion ?? "unknown"), expected \(canonical.appVersion)."
            )
        }
        if snapshot.clientRepairNeeded {
            failures.append("A linked client still needs repair.")
        }
        if !isCanonical(snapshot.shellLink, executable: canonical.executableURL) {
            failures.append("The shell command does not point at \(canonical.executableURL.path).")
        }
        if snapshot.service.daemonVersion != canonical.appVersion {
            failures.append(
                "The login service reports version \(snapshot.service.daemonVersion ?? "unknown"), expected \(canonical.appVersion)."
            )
        }
        if snapshot.service.daemonExecutable.map({ samePath($0, canonical.executableURL) }) != true {
            failures.append(
                "The daemon runs from \(snapshot.service.daemonExecutable?.path ?? "an unknown path"), expected \(canonical.executableURL.path)."
            )
        }
        if !isAppManagedCurrent(snapshot.service.ownership, canonical: canonical) {
            failures.append("The daemon is not owned by this app build.")
        }
        if !snapshot.shadowInstalls.isEmpty {
            let paths = snapshot.shadowInstalls.map(\.url.path).joined(separator: ", ")
            failures.append("Other Plug installs remain: \(paths).")
        }
        return failures
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
            shadowInstalls: shadowInstalls(
                legacy: legacy,
                service: service,
                canonicalExecutable: app.executableURL
            )
        )
    }

    private func shadowInstalls(
        legacy: LegacyInstallSnapshot,
        service: DaemonServiceSnapshot,
        canonicalExecutable: URL
    ) -> [ShadowInstall] {
        var shadows: [ShadowInstall] = []
        var knownPaths = Set<URL>()
        if let cargo = legacy.cargoBinary?.standardizedFileURL {
            shadows.append(ShadowInstall(kind: .cargo, url: cargo))
            knownPaths.insert(cargo)
        }
        // The shell link stays in `recognizedPaths` so legacy launchd jobs
        // that recorded it can be adopted. Once it points at the bundled
        // executable it is the canonical command, not a competing install.
        let canonicalShellLink: Bool
        if case let .canonical(target) = legacy.shellLink {
            canonicalShellLink = samePath(target, canonicalExecutable)
        } else {
            canonicalShellLink = false
        }
        for path in legacy.recognizedPaths.map(\.standardizedFileURL) {
            guard knownPaths.insert(path).inserted else { continue }
            if canonicalShellLink, path.path.hasSuffix("/.local/bin/plug") { continue }
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

    private func adoptionSnapshot(
        live: DaemonServiceSnapshot,
        leftover: DaemonServiceSnapshot?
    ) -> DaemonServiceSnapshot {
        switch live.ownership {
        case .unmanaged:
            if let leftover, case .recognizedLegacy = leftover.ownership {
                return leftover
            }
            return live
        case .recognizedLegacy, .appManagedCurrent, .appManagedStale, .unknown:
            return live
        }
    }

    private func bootOutHomebrewLegacyIfNeeded(_ snapshot: DaemonServiceSnapshot) async throws {
        switch snapshot.ownership {
        case let .recognizedLegacy(records):
            let hasHomebrewJob = records.contains { record in
                record.programURL.map(LegacyPlugProgram.isHomebrew) == true
            }
            guard hasHomebrewJob else { return }
            try await daemonManager.bootOutRecognizedLegacy(snapshot)
        case .appManagedCurrent, .appManagedStale, .unmanaged:
            return
        case .unknown:
            throw CoordinatorError.unknownOwnership
        }
    }

    private func programURLs(from snapshot: DaemonServiceSnapshot) -> Set<URL> {
        switch snapshot.ownership {
        case let .recognizedLegacy(records), let .unknown(records):
            return Set(records.compactMap { $0.programURL?.standardizedFileURL })
        case let .appManagedCurrent(record), let .appManagedStale(record):
            return Set([record.programURL].compactMap { $0?.standardizedFileURL })
        case .unmanaged:
            return []
        }
    }

    private func legacySnapshot(
        _ snapshot: LegacyInstallSnapshot,
        formulaInstalled: Bool? = nil,
        shellLink: ShellLinkState? = nil,
        recognizedPaths: Set<URL>? = nil
    ) -> LegacyInstallSnapshot {
        LegacyInstallSnapshot(
            formulaInstalled: formulaInstalled ?? snapshot.formulaInstalled,
            cargoBinary: snapshot.cargoBinary,
            cargoBinaryIdentity: snapshot.cargoBinaryIdentity,
            shellLink: shellLink ?? snapshot.shellLink,
            recognizedPaths: recognizedPaths ?? snapshot.recognizedPaths,
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
              handshake.daemonExecutable.map({ samePath($0, canonical.executableURL) }) == true,
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
