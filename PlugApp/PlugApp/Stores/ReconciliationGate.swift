import Foundation
import Observation

@MainActor
protocol InstallationCoordinating: AnyObject {
    var state: InstallationState { get }
    func reconcile(trigger: ReconciliationTrigger) async
    func restartService() async throws
    func stopService() async throws
    func openLog()
}

@MainActor
extension InstallationCoordinator: InstallationCoordinating {}

/// Everything that changes the installation or the daemon under the app:
/// reconciliation, a restart, and turning Plug on or off. One runs at a time,
/// and reads of the daemon wait while one is changing it.
@MainActor @Observable
final class ReconciliationGate {
    private let coordinator: any InstallationCoordinating
    private let marker: ServiceDisabledMarker
    private(set) var serviceEnabled: Bool
    private(set) var isChangingService = false
    private(set) var isRestartingService = false
    private var task: Task<Void, Never>?
    private var inFlight = false

    init(coordinator: any InstallationCoordinating, marker: ServiceDisabledMarker) {
        self.coordinator = coordinator
        self.marker = marker
        serviceEnabled = !marker.isSet
    }

    /// Read live rather than mirrored. A copy refreshed only when a
    /// reconciliation ends reports the state the app was in before the work
    /// started, which is how a repair in progress came to describe itself with
    /// the pre-repair situation.
    var state: InstallationState { coordinator.state }

    var isInFlight: Bool { inFlight }

    /// The installation is being looked at, not changed, so reading the
    /// daemon alongside is safe.
    var isOnlyInspecting: Bool {
        if case .reconcilingUpdate(.inspecting) = state { return true }
        return false
    }

    var readsAllowed: Bool { serviceEnabled && !isChangingService && (!inFlight || isOnlyInspecting) }

    var setup: PlugSituation.Setup {
        if !serviceEnabled { return .ready }
        switch state {
        case .healthy: return .ready
        case .adoptionRequired: return .needsPermission
        // The first pass only reads the installation, and every launch runs it.
        // Nothing is being set up there, so setup keeps quiet and the runtime
        // verdict says the true thing: Plug is starting. The later phases do
        // change the installation, and those are worth a word.
        case .reconcilingUpdate(.inspecting), .reconcilingUpdate(.waitingToRetry): return .ready
        case .reconcilingUpdate: return .settingUp
        case let .repairableDrift(drift): return .needsRepair(detail: drift.detail)
        case let .blocked(failure): return .blocked(detail: failure.detail, hasLog: failure.logURL != nil)
        }
    }

    func reconcile(trigger: ReconciliationTrigger) async {
        guard serviceEnabled, !isChangingService else { return }
        await run { [coordinator] in await coordinator.reconcile(trigger: trigger) }
    }

    func openLog() { coordinator.openLog() }

    /// Swaps the daemon for a fresh one. A reconciliation already running
    /// would swallow this one, so it finishes first and the restart that was
    /// asked for still happens. A failed restart is reported and then
    /// repaired by a retry.
    func restart(reporting failure: @escaping @MainActor (any Error) -> Void) async {
        guard serviceEnabled, !isChangingService, !isRestartingService else { return }
        isRestartingService = true
        if let task { await task.value }
        await run { [coordinator] in
            do {
                try await coordinator.restartService()
            } catch {
                failure(error)
                await coordinator.reconcile(trigger: .retry)
            }
        }
        isRestartingService = false
    }

    /// Turns Plug on or off once nothing else is changing it. `whenQuiet`
    /// runs first, for the caller's own reads to finish. Turning on is
    /// explicit consent to register the app's own daemon.
    func setServiceEnabled(_ enabled: Bool, whenQuiet: () async -> Void) async throws {
        guard enabled != serviceEnabled, !isChangingService else { return }
        isChangingService = true
        defer { isChangingService = false }
        if let task { await task.value }
        await whenQuiet()
        if enabled {
            try marker.clear()
            serviceEnabled = true
            await coordinator.reconcile(trigger: .explicitAdoption)
        } else {
            try marker.set()
            do { try await coordinator.stopService() } catch {
                try marker.clear()
                throw error
            }
            serviceEnabled = false
        }
    }

    /// A second caller waits for the one running and does not start another.
    private func run(_ operation: @escaping @MainActor () async -> Void) async {
        if let task {
            await task.value
            return
        }
        inFlight = true
        let task = Task { @MainActor [weak self] in
            await operation()
            self?.inFlight = false
            self?.task = nil
        }
        self.task = task
        await task.value
    }
}
