import Foundation
import Observation
import PlugIPC

@MainActor
protocol InstallationCoordinating: AnyObject {
    var state: InstallationState { get }
    func reconcile(trigger: ReconciliationTrigger) async
    func adopt() async
    func retry() async
    func openLog()
}

@MainActor
extension InstallationCoordinator: InstallationCoordinating {}

@MainActor @Observable
final class AppModel {
    enum ConnectionState: Equatable { case disconnected, connecting, incompatible, ready }

    static let defaultClientVersion =
        (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String)
            .flatMap { $0.isEmpty ? nil : $0 }
            ?? "development"

    static let reconciliationNoticeDelay = Duration.milliseconds(300)

    struct ServerPresentation: Identifiable, Equatable {
        let configured: ConfiguredServer
        let runtime: ServerStatus?
        var id: String { configured.name }
        var health: String { configured.enabled ? (runtime?.health ?? "Starting") : "Disabled" }
        var toolCount: Int { runtime?.toolCount ?? 0 }
    }

    private let ipc: PlugIPCClient
    private let coordinator: any InstallationCoordinating
    private let clientVersion: String
    private var monitoringTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    private var reconciliationTask: Task<Void, Never>?
    private var progressTask: Task<Void, Never>?
    private var hasStarted = false
    private var reconciliationInFlight = false
    private var attemptedSkewRecovery = false
    private(set) var connectionState: ConnectionState = .disconnected
    private(set) var snapshot: OperatorSnapshot = .empty
    private(set) var activities: [ActivityEvent] = []
    private(set) var lastError: String?
    private(set) var installationState: InstallationState
    private(set) var showsReconciliationProgress = false
    private(set) var signingInServers: Set<String> = []

    init(
        ipc: PlugIPCClient? = nil,
        coordinator: any InstallationCoordinating = InstallationCoordinator(),
        clientVersion: String = AppModel.defaultClientVersion
    ) {
        self.clientVersion = clientVersion
        self.ipc = ipc ?? PlugIPCClient(clientVersion: clientVersion)
        self.coordinator = coordinator
        installationState = coordinator.state
    }

    var visibleServers: [ServerPresentation] {
        let runtimeByName = Dictionary(uniqueKeysWithValues: snapshot.servers.map { ($0.serverId, $0) })
        return snapshot.configuredServers.map {
            ServerPresentation(configured: $0, runtime: runtimeByName[$0.name])
        }.enumerated().sorted {
            let lhsBad = $0.element.health != "Healthy"
            let rhsBad = $1.element.health != "Healthy"
            return lhsBad == rhsBad ? $0.offset < $1.offset : lhsBad && !rhsBad
        }.map(\.element)
    }

    var menuBarSymbol: String {
        if connectionState != .ready { return "bolt.slash.circle" }
        return visibleServers.contains { $0.health == "Failed" || $0.health == "AuthRequired" }
            ? "bolt.trianglebadge.exclamationmark" : "bolt.circle.fill"
    }

    var isHealthy: Bool {
        connectionState == .ready && visibleServers.allSatisfy {
            !$0.configured.enabled || $0.health == "Healthy"
        }
    }

    var installationFailure: InstallationFailure? {
        guard case let .blocked(failure) = installationState else { return nil }
        return failure
    }

    var installationDrift: InstallationDrift? {
        guard case let .repairableDrift(drift) = installationState else { return nil }
        return drift
    }

    var adoptionIsRequired: Bool {
        if case .adoptionRequired = installationState { return true }
        return false
    }

    func start() async {
        guard !hasStarted else { return }
        hasStarted = true
        await reconcile(trigger: .applicationLaunch)
        guard !Task.isCancelled else { return }
        await refresh()
        guard !Task.isCancelled else { return }

        monitoringTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                guard !Task.isCancelled else { return }
                await self?.refresh()
            }
        }
    }

    func reconcile(trigger: ReconciliationTrigger) async {
        await runReconciliation { [coordinator] in
            await coordinator.reconcile(trigger: trigger)
        }
    }

    func adopt() async {
        await runReconciliation { [coordinator] in
            await coordinator.adopt()
        }
    }

    func retry() async {
        await runReconciliation { [coordinator] in
            await coordinator.retry()
        }
    }

    func openLog() {
        coordinator.openLog()
    }

    func refresh() async {
        guard refreshTask == nil, !reconciliationInFlight else { return }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            if connectionState != .ready { connectionState = .connecting }
            do {
                let handshake = try await ipc.connect()
                guard handshake.ipcMin <= 4, handshake.ipcMax >= 3 else {
                    connectionState = .incompatible
                    return
                }
                guard handshake.daemonVersion == clientVersion else {
                    connectionState = .incompatible
                    if !attemptedSkewRecovery {
                        attemptedSkewRecovery = true
                        await retry()
                    }
                    return
                }
                attemptedSkewRecovery = false
                let token = try String(contentsOf: PlugIPCClient.defaultTokenURL, encoding: .utf8)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                guard case let .snapshot(value) = try await ipc.request(.snapshot(authToken: token)) else {
                    throw PlugIPCError.unexpectedResponse("OperatorSnapshot")
                }
                snapshot = value
                NotificationService.shared.observe(value)
                if case let .activity(events) = try await ipc.request(
                    .activity(authToken: token, afterSequence: 0, limit: 200, failuresOnly: false)
                ) { activities = events }
                connectionState = .ready
                lastError = nil
            } catch {
                connectionState = .disconnected
                lastError = error.localizedDescription
            }
        }
        refreshTask = task
        await task.value
        refreshTask = nil
    }

    func perform(_ request: (String) -> IPCRequest) async {
        do {
            let token = try String(contentsOf: PlugIPCClient.defaultTokenURL, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            _ = try await ipc.request(request(token))
            await refresh()
        } catch { lastError = error.localizedDescription }
    }

    func signIn(server: String) async {
        guard signingInServers.insert(server).inserted else { return }
        defer { signingInServers.remove(server) }
        do {
            try await AuthFlowService().signIn(server: server)
            await refresh()
        } catch { lastError = error.localizedDescription }
    }

    private func runReconciliation(_ operation: @escaping @MainActor () async -> Void) async {
        if let reconciliationTask {
            await reconciliationTask.value
            return
        }

        reconciliationInFlight = true
        showsReconciliationProgress = false
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            self.progressTask = Task { @MainActor [weak self] in
                do {
                    try await Task.sleep(for: Self.reconciliationNoticeDelay)
                    guard !Task.isCancelled else { return }
                    self?.showsReconciliationProgress = true
                } catch { }
            }
            await operation()
            self.installationState = self.coordinator.state
            self.progressTask?.cancel()
            self.progressTask = nil
            self.showsReconciliationProgress = false
            self.reconciliationInFlight = false
            self.reconciliationTask = nil
        }
        reconciliationTask = task
        await task.value
    }
}
