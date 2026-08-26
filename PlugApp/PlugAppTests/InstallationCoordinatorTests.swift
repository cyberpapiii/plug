import Foundation
import PlugIPC
import XCTest
@testable import Plug

@MainActor
final class InstallationCoordinatorTests: XCTestCase {
    private let canonical = VerifiedAppInstallation(
        bundleURL: URL(fileURLWithPath: "/Applications/Plug.app"),
        executableURL: URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"),
        appVersion: "0.7.0",
        buildVersion: "20",
        embeddedVersion: "0.7.0",
        teamID: "HJF7LN64XX"
    )

    func testRoutineReconciliationRunsOrderedProofAndPublishesHealthy() async {
        let events = EventLog()
        let cargo = URL(fileURLWithPath: "/Users/me/.cargo/bin/plug")
        let initialLegacy = LegacyInstallSnapshot(
            formulaInstalled: true,
            cargoBinary: cargo,
            shellLink: .repairable(cargo),
            recognizedPaths: [cargo],
            unknownPaths: []
        )
        let finalLegacy = LegacyInstallSnapshot(
            formulaInstalled: false,
            cargoBinary: nil,
            shellLink: .canonical(canonical.executableURL),
            recognizedPaths: [],
            unknownPaths: []
        )
        let current = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: canonical.executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: canonical.buildVersion,
            loaded: true
        )
        let service = DaemonServiceSnapshot(
            ownership: .appManagedCurrent(current),
            daemonVersion: canonical.appVersion,
            daemonExecutable: canonical.executableURL
        )
        let app = RecordingAppInspector(events: events, values: [canonical, canonical])
        let legacy = RecordingLegacyMigrator(events: events, values: [initialLegacy, finalLegacy])
        let clients = RecordingClientRepairer(events: events, values: [true, false])
        let daemon = RecordingDaemonManager(
            events: events,
            inspections: [service, service],
            handshakes: [handshake(version: canonical.appVersion)]
        )
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: clients,
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        let eventValues = await events.values
        XCTAssertEqual(
            eventValues,
            [
                "app.inspect",
                "legacy.inspect",
                "legacy.removeFormula",
                "legacy.repairShell",
                "clients.inspect",
                "clients.repair",
                "daemon.inspect",
                "daemon.ensureRunning",
                "legacy.removeCargo",
                "app.inspect",
                "legacy.inspect",
                "clients.inspect",
                "daemon.inspect",
            ]
        )
        guard case let .healthy(snapshot) = coordinator.state else {
            return XCTFail("Expected healthy state, got \(coordinator.state)")
        }
        XCTAssertEqual(snapshot.app, canonical)
        XCTAssertEqual(snapshot.shellLink, .canonical(canonical.executableURL))
        XCTAssertEqual(snapshot.daemonVersion, canonical.appVersion)
        XCTAssertFalse(snapshot.clientRepairNeeded)
    }

    func testLegacyDaemonRequiresExplicitAdoptionAndDoesNotMutateDuringLaunch() async {
        let events = EventLog()
        let legacyRecord = LaunchdJobRecord(
            label: "local.claude-rc.plug",
            programURL: URL(fileURLWithPath: "/Users/me/.cargo/bin/plug"),
            parentBundleIdentifier: nil,
            parentBundleVersion: nil,
            loaded: true
        )
        let service = DaemonServiceSnapshot(
            ownership: .recognizedLegacy([legacyRecord]),
            daemonVersion: "0.6.4",
            daemonExecutable: legacyRecord.programURL
        )
        let app = RecordingAppInspector(events: events, values: [canonical])
        let legacy = RecordingLegacyMigrator(events: events, values: [emptyLegacy()])
        let clients = RecordingClientRepairer(events: events, values: [false])
        let daemon = RecordingDaemonManager(events: events, inspections: [service])
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: clients,
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case .adoptionRequired = coordinator.state else {
            return XCTFail("Expected adoptionRequired, got \(coordinator.state)")
        }
        let eventValues = await events.values
        XCTAssertFalse(eventValues.contains("daemon.adoptLegacy"))
    }

    func testEnabledAppServiceAutomaticallyReclaimsLegacyDaemon() async {
        let events = EventLog()
        let legacyRecord = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: URL(fileURLWithPath: "/Users/me/.local/bin/plug"),
            parentBundleIdentifier: nil,
            parentBundleVersion: nil,
            loaded: true
        )
        let legacyService = DaemonServiceSnapshot(
            ownership: .recognizedLegacy([legacyRecord]),
            daemonVersion: canonical.appVersion,
            daemonExecutable: legacyRecord.programURL
        )
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [emptyLegacy(), emptyLegacy()]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [legacyService, healthyService()],
                handshakes: [handshake(version: canonical.appVersion)],
                appServiceEnabled: true
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case .healthy = coordinator.state else {
            return XCTFail("Expected automatic recovery after prior app-service consent, got \(coordinator.state)")
        }
        let eventValues = await events.values
        XCTAssertTrue(eventValues.contains("daemon.adoptLegacy"))
    }

    func testEnabledAppServiceAutomaticallyRecoversMissingJob() async {
        let events = EventLog()
        let unmanaged = DaemonServiceSnapshot(
            ownership: .unmanaged,
            daemonVersion: nil,
            daemonExecutable: nil
        )
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [emptyLegacy(), emptyLegacy()]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [unmanaged, healthyService()],
                handshakes: [handshake(version: canonical.appVersion)],
                appServiceEnabled: true
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case .healthy = coordinator.state else {
            return XCTFail("Expected automatic recovery for enabled app service, got \(coordinator.state)")
        }
        let eventValues = await events.values
        XCTAssertTrue(eventValues.contains("daemon.adopt"))
        XCTAssertTrue(eventValues.contains("daemon.ensureRunning"))
    }

    func testExplicitAdoptionConvergesAfterRequiredState() async {
        let events = EventLog()
        let legacyRecord = LaunchdJobRecord(
            label: "local.claude-rc.plug",
            programURL: URL(fileURLWithPath: "/Users/me/.cargo/bin/plug"),
            parentBundleIdentifier: nil,
            parentBundleVersion: nil,
            loaded: true
        )
        let legacyService = DaemonServiceSnapshot(
            ownership: .recognizedLegacy([legacyRecord]),
            daemonVersion: "0.6.4",
            daemonExecutable: legacyRecord.programURL
        )
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [emptyLegacy(), emptyLegacy()]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [legacyService, legacyService, healthyService()],
                handshakes: [handshake(version: canonical.appVersion)]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)
        await coordinator.adopt()

        guard case .healthy = coordinator.state else {
            return XCTFail("Expected explicit adoption to converge, got \(coordinator.state)")
        }
        let eventValues = await events.values
        XCTAssertTrue(eventValues.contains("daemon.adoptLegacy"))
    }

    func testStaleAppServiceUsesAutomaticReplacement() async {
        let events = EventLog()
        let staleRecord = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: canonical.executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: "19",
            loaded: true
        )
        let staleService = DaemonServiceSnapshot(
            ownership: .appManagedStale(staleRecord),
            daemonVersion: "0.6.4",
            daemonExecutable: canonical.executableURL
        )
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [emptyLegacy(), emptyLegacy()]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [staleService, healthyService()],
                handshakes: [handshake(version: canonical.appVersion)]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case .healthy = coordinator.state else {
            return XCTFail("Expected stale service replacement to converge, got \(coordinator.state)")
        }
        let eventValues = await events.values
        XCTAssertTrue(eventValues.contains("daemon.replaceStale"))
        XCTAssertFalse(eventValues.contains("daemon.ensureRunning"))
    }

    func testWrongHandshakeStopsBeforeFinalHealthyPublication() async {
        let events = EventLog()
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [emptyLegacy(), emptyLegacy()]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [healthyService()],
                handshakes: [handshake(version: "0.6.4")]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        if case .healthy = coordinator.state {
            XCTFail("Wrong daemon handshake must not report healthy")
        }
        let eventValues = await events.values
        XCTAssertEqual(eventValues.filter { $0 == "app.inspect" }.count, 1)
    }

    func testHandshakeOwnershipMismatchPreservesCargoAndDoesNotReportHealthy() async {
        let events = EventLog()
        let cargo = URL(fileURLWithPath: "/Users/me/.cargo/bin/plug")
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [
                    LegacyInstallSnapshot(
                        formulaInstalled: false,
                        cargoBinary: cargo,
                        shellLink: .canonical(canonical.executableURL),
                        recognizedPaths: [cargo],
                        unknownPaths: []
                    ),
                    emptyLegacy(),
                ]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [healthyService(), healthyService()],
                handshakes: [handshake(version: canonical.appVersion, ownership: "cli_managed")]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        if case .healthy = coordinator.state {
            XCTFail("A non-app-managed handshake must not report healthy")
        }
        let eventValues = await events.values
        XCTAssertFalse(eventValues.contains("legacy.removeCargo"))
    }

    func testIncompatibleHandshakePreservesCargoAndDoesNotReportHealthy() async {
        let events = EventLog()
        let cargo = URL(fileURLWithPath: "/Users/me/.cargo/bin/plug")
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [
                    LegacyInstallSnapshot(
                        formulaInstalled: false,
                        cargoBinary: cargo,
                        shellLink: .canonical(canonical.executableURL),
                        recognizedPaths: [cargo],
                        unknownPaths: []
                    ),
                    emptyLegacy(),
                ]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [healthyService(), healthyService()],
                handshakes: [handshake(
                    version: canonical.appVersion,
                    ipcMin: 6,
                    ipcMax: 6
                )]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        if case .healthy = coordinator.state {
            XCTFail("An incompatible handshake must not report healthy")
        }
        let eventValues = await events.values
        XCTAssertFalse(eventValues.contains("legacy.removeCargo"))
    }

    func testConcurrentReconciliationCallsShareOneInFlightOperation() async {
        let events = EventLog()
        let gate = AsyncGate()
        let app = BlockingAppInspector(events: events, value: canonical, gate: gate)
        let daemon = RecordingDaemonManager(
            events: events,
            inspections: [healthyService(), healthyService()]
        )
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: RecordingLegacyMigrator(events: events, values: [emptyLegacy()]),
            clientRepairer: RecordingClientRepairer(events: events, values: [false]),
            daemonManager: daemon,
            openURL: { _ in }
        )

        let first = Task { await coordinator.reconcile(trigger: .applicationLaunch) }
        await gate.waitUntilEntered()
        let second = Task { await coordinator.reconcile(trigger: .retry) }
        await Task.yield()
        let callsWhileBlocked = await app.calls
        XCTAssertEqual(callsWhileBlocked, 1)
        await gate.release()
        await first.value
        await second.value

        let callsAfterCoalescing = await app.calls
        XCTAssertEqual(callsAfterCoalescing, 2)
    }

    func testBlockedStateDoesNotSelfRetryUntilExplicitRetry() async {
        let events = EventLog()
        let app = FailingAppInspector(events: events, error: TestFailure.operational)
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: RecordingLegacyMigrator(events: events, values: [emptyLegacy()]),
            clientRepairer: RecordingClientRepairer(events: events, values: [false]),
            daemonManager: RecordingDaemonManager(events: events),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)
        await coordinator.reconcile(trigger: .applicationLaunch)
        let callsBeforeRetry = await app.calls
        XCTAssertEqual(callsBeforeRetry, 1)
        guard case .blocked = coordinator.state else {
            return XCTFail("Expected blocked state, got \(coordinator.state)")
        }

        await coordinator.retry()
        let callsAfterRetry = await app.calls
        XCTAssertEqual(callsAfterRetry, 2)
    }

    func testUnrelatedShellFileBecomesRepairableDriftWithoutMutation() async {
        let events = EventLog()
        let unrelated = URL(fileURLWithPath: "/Users/me/.local/bin/plug")
        let app = RecordingAppInspector(events: events, values: [canonical])
        let legacy = RecordingLegacyMigrator(
            events: events,
            values: [LegacyInstallSnapshot(
                formulaInstalled: false,
                cargoBinary: nil,
                shellLink: .unrelated(unrelated),
                recognizedPaths: [],
                unknownPaths: [unrelated]
            )]
        )
        let daemon = RecordingDaemonManager(events: events)
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: RecordingClientRepairer(events: events, values: [false]),
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .repairableDrift(drift) = coordinator.state else {
            return XCTFail("Expected repairable drift, got \(coordinator.state)")
        }
        XCTAssertTrue(drift.detail.contains(unrelated.path))
        let eventValues = await events.values
        XCTAssertFalse(eventValues.contains("legacy.repairShell"))
        XCTAssertFalse(eventValues.contains("daemon.inspect"))
    }

    func testFinalDisagreementNeverReportsHealthy() async {
        let events = EventLog()
        let current = healthyService()
        let staleRecord = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: canonical.executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: "19",
            loaded: true
        )
        let stale = DaemonServiceSnapshot(
            ownership: .appManagedStale(staleRecord),
            daemonVersion: canonical.appVersion,
            daemonExecutable: canonical.executableURL
        )
        let app = RecordingAppInspector(events: events, values: [canonical, canonical])
        let legacy = RecordingLegacyMigrator(events: events, values: [emptyLegacy(), emptyLegacy()])
        let clients = RecordingClientRepairer(events: events, values: [false, false])
        let daemon = RecordingDaemonManager(
            events: events,
            inspections: [current, stale],
            handshakes: [handshake(version: canonical.appVersion)]
        )
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: clients,
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        if case .healthy = coordinator.state {
            XCTFail("Final launchd disagreement must not report healthy")
        }
    }

    func testCanonicalShellLinkInRecognizedPathsStillConvergesHealthy() async {
        // `LegacyInstallMigrator` keeps the repaired shell link in
        // `recognizedPaths` so legacy launchd jobs can be adopted. That link is
        // the canonical command, not a competing install.
        let events = EventLog()
        let shellLink = URL(fileURLWithPath: "/Users/me/.local/bin/plug")
        let legacySnapshot = LegacyInstallSnapshot(
            formulaInstalled: false,
            cargoBinary: nil,
            shellLink: .canonical(canonical.executableURL),
            recognizedPaths: [shellLink],
            unknownPaths: []
        )
        let service = healthyService()
        let app = RecordingAppInspector(events: events, values: [canonical, canonical])
        let legacy = RecordingLegacyMigrator(events: events, values: [legacySnapshot, legacySnapshot])
        let clients = RecordingClientRepairer(events: events, values: [false, false])
        let daemon = RecordingDaemonManager(
            events: events,
            inspections: [service, service],
            handshakes: [handshake(version: canonical.appVersion)]
        )
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: clients,
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .healthy(snapshot) = coordinator.state else {
            return XCTFail("Canonical shell link must not block convergence, got \(coordinator.state)")
        }
        XCTAssertTrue(snapshot.shadowInstalls.isEmpty)
        XCTAssertEqual(snapshot.shellLink, .canonical(canonical.executableURL))
    }

    func testRepairableShellLinkInRecognizedPathsRemainsShadowInstall() async {
        let events = EventLog()
        let shellLink = URL(fileURLWithPath: "/Users/me/.local/bin/plug")
        let cargo = URL(fileURLWithPath: "/Users/me/.cargo/bin/plug")
        let legacySnapshot = LegacyInstallSnapshot(
            formulaInstalled: false,
            cargoBinary: nil,
            shellLink: .repairable(cargo),
            recognizedPaths: [shellLink],
            unknownPaths: []
        )
        let service = healthyService()
        let app = RecordingAppInspector(events: events, values: [canonical, canonical])
        let legacy = RecordingLegacyMigrator(events: events, values: [legacySnapshot, legacySnapshot])
        let clients = RecordingClientRepairer(events: events, values: [false, false])
        let daemon = RecordingDaemonManager(
            events: events,
            inspections: [service, service],
            handshakes: [handshake(version: canonical.appVersion)]
        )
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: clients,
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .repairableDrift(drift) = coordinator.state else {
            return XCTFail("Unrepaired shell link must not report healthy, got \(coordinator.state)")
        }
        XCTAssertTrue(drift.detail.contains(shellLink.path), drift.detail)
    }

    func testFinalDisagreementDetailNamesTheFailedCheck() async {
        let events = EventLog()
        let current = healthyService()
        let staleRecord = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: canonical.executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: "19",
            loaded: true
        )
        let stale = DaemonServiceSnapshot(
            ownership: .appManagedStale(staleRecord),
            daemonVersion: "0.0.1",
            daemonExecutable: canonical.executableURL
        )
        let app = RecordingAppInspector(events: events, values: [canonical, canonical])
        let legacy = RecordingLegacyMigrator(events: events, values: [emptyLegacy(), emptyLegacy()])
        let clients = RecordingClientRepairer(events: events, values: [false, false])
        let daemon = RecordingDaemonManager(
            events: events,
            inspections: [current, stale],
            handshakes: [handshake(version: canonical.appVersion)]
        )
        let coordinator = InstallationCoordinator(
            appInspector: app,
            legacyMigrator: legacy,
            clientRepairer: clients,
            daemonManager: daemon,
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .repairableDrift(drift) = coordinator.state else {
            return XCTFail("Expected repairable drift, got \(coordinator.state)")
        }
        XCTAssertTrue(drift.detail.contains("not owned by this app build"), drift.detail)
        XCTAssertTrue(drift.detail.contains("0.0.1"), drift.detail)
        XCTAssertFalse(drift.detail.contains("shell command"), drift.detail)
    }

    func testFinalHandshakeExecutableMismatchNeverReportsHealthy() async {
        let events = EventLog()
        let wrongExecutable = URL(fileURLWithPath: "/Applications/Other Plug.app/Contents/Resources/plug")
        let finalRecord = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: canonical.executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: canonical.buildVersion,
            loaded: true
        )
        let finalService = DaemonServiceSnapshot(
            ownership: .appManagedCurrent(finalRecord),
            daemonVersion: canonical.appVersion,
            daemonExecutable: wrongExecutable
        )
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(events: events, values: [emptyLegacy(), emptyLegacy()]),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [healthyService(), finalService]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        if case .healthy = coordinator.state {
            XCTFail("Final handshake executable mismatch must not report healthy")
        }
    }

    func testFinalUnknownDaemonOwnershipBlocksInsteadOfReportingRepairableDrift() async {
        let events = EventLog()
        let coordinator = InstallationCoordinator(
            appInspector: RecordingAppInspector(events: events, values: [canonical, canonical]),
            legacyMigrator: RecordingLegacyMigrator(
                events: events,
                values: [emptyLegacy(), emptyLegacy()]
            ),
            clientRepairer: RecordingClientRepairer(events: events, values: [false, false]),
            daemonManager: RecordingDaemonManager(
                events: events,
                inspections: [
                    healthyService(),
                    DaemonServiceSnapshot(
                        ownership: .unknown([
                            LaunchdJobRecord(
                                label: "com.plug.daemon",
                                programURL: URL(fileURLWithPath: "/Applications/Other.app/plug"),
                                parentBundleIdentifier: "com.example.other",
                                parentBundleVersion: "1",
                                loaded: true
                            ),
                        ]),
                        daemonVersion: nil,
                        daemonExecutable: nil
                    ),
                ],
                handshakes: [handshake(version: canonical.appVersion)]
            ),
            openURL: { _ in }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .blocked(failure) = coordinator.state else {
            return XCTFail("Unknown final ownership must block, got \(coordinator.state)")
        }
        XCTAssertEqual(failure.summary, "Plug daemon ownership is unknown")
    }

    func testOpenLogUsesBlockedFailureLogURL() async {
        let events = EventLog()
        let opened = OpenedURL()
        let logURL = URL(fileURLWithPath: "/tmp/plug-reconciliation.log")
        let coordinator = InstallationCoordinator(
            appInspector: FailingAppInspector(events: events, error: TestFailure.operational),
            legacyMigrator: RecordingLegacyMigrator(events: events, values: [emptyLegacy()]),
            clientRepairer: RecordingClientRepairer(events: events, values: [false]),
            daemonManager: RecordingDaemonManager(events: events),
            logURL: logURL,
            openURL: { url in opened.value = url }
        )

        await coordinator.reconcile(trigger: .applicationLaunch)
        coordinator.openLog()

        XCTAssertEqual(opened.value, logURL)
    }

    private func emptyLegacy() -> LegacyInstallSnapshot {
        LegacyInstallSnapshot(
            formulaInstalled: false,
            cargoBinary: nil,
            shellLink: .canonical(canonical.executableURL),
            recognizedPaths: [],
            unknownPaths: []
        )
    }

    private func healthyService() -> DaemonServiceSnapshot {
        let record = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: canonical.executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: canonical.buildVersion,
            loaded: true
        )
        return DaemonServiceSnapshot(
            ownership: .appManagedCurrent(record),
            daemonVersion: canonical.appVersion,
            daemonExecutable: canonical.executableURL
        )
    }

    private func handshake(
        version: String,
        ipcMin: UInt16 = 3,
        ipcMax: UInt16 = 4,
        ownership: String = "app_managed"
    ) -> OperatorHandshake {
        let data = try! JSONSerialization.data(withJSONObject: [
            "daemonVersion": version,
            "daemonExecutable": canonical.executableURL.path,
            "ipcMin": ipcMin,
            "ipcMax": ipcMax,
            "ownership": ownership,
            "capabilities": [],
        ])
        return try! JSONDecoder().decode(OperatorHandshake.self, from: data)
    }
}

private actor EventLog {
    private(set) var values: [String] = []

    func append(_ value: String) { values.append(value) }
}

private actor RecordingAppInspector: AppInstallationInspecting {
    private let events: EventLog
    private var values: [VerifiedAppInstallation]
    private(set) var calls = 0

    init(events: EventLog, values: [VerifiedAppInstallation]) {
        self.events = events
        self.values = values
    }

    func inspectCurrentApp() async throws -> VerifiedAppInstallation {
        await events.append("app.inspect")
        calls += 1
        return values.count > 1 ? values.removeFirst() : values[0]
    }
}

private actor BlockingAppInspector: AppInstallationInspecting {
    private let events: EventLog
    private let value: VerifiedAppInstallation
    private let gate: AsyncGate
    private(set) var calls = 0

    init(events: EventLog, value: VerifiedAppInstallation, gate: AsyncGate) {
        self.events = events
        self.value = value
        self.gate = gate
    }

    func inspectCurrentApp() async throws -> VerifiedAppInstallation {
        calls += 1
        await events.append("app.inspect")
        await gate.enter()
        return value
    }
}

private actor FailingAppInspector: AppInstallationInspecting {
    private let events: EventLog
    private let error: Error
    private(set) var calls = 0

    init(events: EventLog, error: Error) {
        self.events = events
        self.error = error
    }

    func inspectCurrentApp() async throws -> VerifiedAppInstallation {
        calls += 1
        await events.append("app.inspect")
        throw error
    }
}

private actor RecordingLegacyMigrator: LegacyInstallMigrating {
    private let events: EventLog
    private var values: [LegacyInstallSnapshot]

    init(events: EventLog, values: [LegacyInstallSnapshot]) {
        self.events = events
        self.values = values
    }

    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot {
        await events.append("legacy.inspect")
        return values.count > 1 ? values.removeFirst() : values[0]
    }

    func removeRecognizedFormula(_ snapshot: LegacyInstallSnapshot) async throws {
        await events.append("legacy.removeFormula")
    }

    func repairShellLink(to executable: URL) async throws -> ShellLinkState {
        await events.append("legacy.repairShell")
        return .canonical(executable)
    }

    func removeVerifiedCargoBinary(
        _ snapshot: LegacyInstallSnapshot,
        proof: ReconciliationProof
    ) async throws {
        await events.append("legacy.removeCargo")
    }
}

private actor RecordingClientRepairer: ClientRepairing {
    private let events: EventLog
    private var values: [Bool]

    init(events: EventLog, values: [Bool]) {
        self.events = events
        self.values = values
    }

    func inspect(canonicalExecutable: URL) async throws -> Bool {
        await events.append("clients.inspect")
        return values.count > 1 ? values.removeFirst() : values[0]
    }

    func repairAll(canonicalExecutable: URL) async throws -> ClientRepairResult {
        await events.append("clients.repair")
        return ClientRepairResult(examined: 1, repaired: 1, unchanged: 0)
    }
}

@MainActor
private final class RecordingDaemonManager: DaemonServiceManaging {
    private let events: EventLog
    private var inspections: [DaemonServiceSnapshot]
    private var handshakes: [OperatorHandshake]
    let appServiceEnabled: Bool

    init(
        events: EventLog,
        inspections: [DaemonServiceSnapshot] = [],
        handshakes: [OperatorHandshake] = [],
        appServiceEnabled: Bool = false
    ) {
        self.events = events
        self.inspections = inspections
        self.handshakes = handshakes
        self.appServiceEnabled = appServiceEnabled
    }

    func inspect(
        canonical: VerifiedAppInstallation,
        legacyPaths: Set<URL>
    ) async throws -> DaemonServiceSnapshot {
        await events.append("daemon.inspect")
        if inspections.count > 1 { return inspections.removeFirst() }
        return inspections.first ?? DaemonServiceSnapshot(ownership: .unmanaged, daemonVersion: nil, daemonExecutable: nil)
    }

    func adoptRecognizedLegacy(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        await events.append("daemon.adoptLegacy")
        return handshakes.removeFirst()
    }

    func replaceStaleAppService(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        await events.append("daemon.replaceStale")
        return handshakes.removeFirst()
    }

    func ensureRunning(expectedVersion: String) async throws -> OperatorHandshake {
        await events.append("daemon.ensureRunning")
        return handshakes.isEmpty
            ? makeHandshake(version: expectedVersion)
            : handshakes.removeFirst()
    }

    func adopt() async throws {
        await events.append("daemon.adopt")
    }
}

private func makeHandshake(version: String) -> OperatorHandshake {
    let data = try! JSONSerialization.data(withJSONObject: [
        "daemonVersion": version,
        "daemonExecutable": "/Applications/Plug.app/Contents/Resources/plug",
        "ipcMin": 3,
        "ipcMax": 4,
        "ownership": "app_managed",
        "capabilities": [],
    ])
    return try! JSONDecoder().decode(OperatorHandshake.self, from: data)
}

private actor AsyncGate {
    private var entered = false
    private var released = false
    private var enteredWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func enter() async {
        entered = true
        enteredWaiters.forEach { $0.resume() }
        enteredWaiters.removeAll()
        if !released {
            await withCheckedContinuation { continuation in
                releaseWaiters.append(continuation)
            }
        }
    }

    func waitUntilEntered() async {
        if entered { return }
        await withCheckedContinuation { continuation in
            enteredWaiters.append(continuation)
        }
    }

    func release() {
        released = true
        releaseWaiters.forEach { $0.resume() }
        releaseWaiters.removeAll()
    }
}

private final class OpenedURL: @unchecked Sendable {
    var value: URL?
}

private enum TestFailure: Error {
    case operational
}
