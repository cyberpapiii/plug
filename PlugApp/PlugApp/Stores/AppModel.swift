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

    static let defaultClientVersion = PlugIPCClient.clientVersion(
        from: Bundle.main.infoDictionary ?? [:]
    )

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
    private let appLinker: any AppLinking
    private let tokenURL: URL
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
    /// How far back the history goes. The daemon keeps a bounded ring, so this
    /// is the whole of what can be asked for, not a page of a longer list.
    static let activityLimit = 200
    /// True when history is long enough to have been cut off at that limit,
    /// which the list says out loud rather than pretending to be complete.
    var activityIsCapped: Bool { activities.count >= Self.activityLimit }
    private(set) var lastError: String?
    private(set) var installationState: InstallationState
    private(set) var showsReconciliationProgress = false
    private(set) var signingInServers: Set<String> = []
    private(set) var toolCatalog = ToolCatalog()
    private(set) var connectableApps: [LinkableApp] = []
    private(set) var busyApps: Set<String> = []
    private var capabilities: Set<String> = []

    /// The daemon accepts per-tool switches. Older daemons do not, and the
    /// interface hides the switches rather than offering a button that fails.
    var canManageTools: Bool { capabilities.contains("tool_mutation") }
    /// Someone is looking at Plug right now, so refresh briskly. Nothing is
    /// visible otherwise, and a background poll every few seconds is rude to
    /// a laptop battery for information no one is reading.
    private var watcherCount = 0

    static let foregroundPollInterval = Duration.seconds(2)
    static let backgroundPollInterval = Duration.seconds(30)

    private var pollInterval: Duration {
        watcherCount > 0 ? Self.foregroundPollInterval : Self.backgroundPollInterval
    }

    /// Called when a surface appears or disappears. Balanced pairs only.
    func setWatching(_ watching: Bool) {
        watcherCount = max(0, watcherCount + (watching ? 1 : -1))
        if watching { Task { await refresh() } }
    }

    init(
        ipc: PlugIPCClient? = nil,
        coordinator: any InstallationCoordinating = InstallationCoordinator(),
        clientVersion: String = AppModel.defaultClientVersion,
        tokenURL: URL = PlugIPCClient.defaultTokenURL,
        appLinker: any AppLinking = AppLinkService()
    ) {
        self.clientVersion = clientVersion
        self.ipc = ipc ?? PlugIPCClient(clientVersion: clientVersion)
        self.coordinator = coordinator
        self.tokenURL = tokenURL
        self.appLinker = appLinker
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

    /// Everything the interface needs to describe Plug, as one plain value.
    var situation: PlugSituation {
        PlugSituation(
            setup: setupState,
            runtime: runtimeState,
            servers: serverFacts,
            connectedApps: snapshot.liveSessions.count,
            version: snapshot.runtimeVersion
        )
    }

    /// The single sentence every surface renders.
    var verdict: Verdict { PlugVerdict.verdict(for: situation) }

    /// Problems paired with the buttons that fix them.
    var attentionItems: [AttentionItem] { PlugVerdict.attention(for: situation) }

    var serverFacts: [ServerFacts] {
        let auth = Dictionary(
            snapshot.upstreamAuth.map { ($0.name, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        return visibleServers.map { server in
            let account = auth[server.configured.name]
            return ServerFacts(
                name: server.configured.name,
                enabled: server.configured.enabled,
                transport: server.configured.transport,
                usesOAuth: server.configured.oauth,
                health: ServerHealth(
                    daemonValue: server.configured.enabled ? server.runtime?.health : "Disabled",
                    enabled: server.configured.enabled
                ),
                toolCount: server.toolCount,
                error: server.runtime?.error,
                isSigningIn: signingInServers.contains(server.configured.name),
                tokenExpiresInSecs: account?.tokenExpiresInSecs,
                authWarnings: account?.warnings ?? []
            )
        }
    }

    private var setupState: PlugSituation.Setup {
        if showsReconciliationProgress { return .settingUp }
        switch installationState {
        case .healthy: return .ready
        case .adoptionRequired: return .needsPermission
        case .reconcilingUpdate: return .settingUp
        case let .repairableDrift(drift): return .needsRepair(detail: drift.detail)
        case let .blocked(failure): return .blocked(detail: failure.detail, hasLog: failure.logURL != nil)
        }
    }

    private var runtimeState: PlugSituation.Runtime {
        switch connectionState {
        case .ready: .running
        case .connecting: .starting
        case .incompatible: .versionMismatch
        case .disconnected: .stopped
        }
    }

    var menuBarSymbol: String { PlugVerdict.menuBarSymbol(for: verdict) }

    var isHealthy: Bool {
        guard case .healthy = installationState, connectionState == .ready else { return false }
        return visibleServers.allSatisfy {
            !$0.configured.enabled || $0.health == "Healthy"
        }
    }

    /// Recent calls that touched one server, newest first.
    func recentActivity(for server: String, limit: Int = 12) -> [ActivityEvent] {
        activities
            .filter { $0.server == server }
            .sorted { $0.sequence > $1.sequence }
            .prefix(limit)
            .map { $0 }
    }

    var connectionRecoveryIsRequired: Bool {
        connectionState == .incompatible
    }

    var connectionRecoveryDetail: String {
        "The app and its background service are running different versions."
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
                let interval = await self?.pollInterval ?? Self.backgroundPollInterval
                try? await Task.sleep(for: interval)
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
        await ipc.disconnect()
        await runReconciliation { [coordinator] in
            await coordinator.retry()
        }
    }

    func retryConnection() async {
        attemptedSkewRecovery = false
        await retry()
        await refresh()
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
                capabilities = Set(handshake.capabilities)
                guard handshake.ipcMin <= 5, handshake.ipcMax >= 3 else {
                    connectionState = .incompatible
                    lastError = nil
                    return
                }
                guard handshake.daemonVersion == clientVersion else {
                    connectionState = .incompatible
                    lastError = nil
                    if !attemptedSkewRecovery {
                        attemptedSkewRecovery = true
                        await retry()
                    }
                    return
                }
                attemptedSkewRecovery = false
                let token = try String(contentsOf: tokenURL, encoding: .utf8)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                guard case let .snapshot(value) = try await ipc.request(.snapshot(authToken: token)) else {
                    throw PlugIPCError.unexpectedResponse("OperatorSnapshot")
                }
                snapshot = value
                NotificationService.shared.observe(value)
                if case let .activity(events) = try await ipc.request(
                    .activity(authToken: token, afterSequence: 0, limit: Self.activityLimit, failuresOnly: false)
                ) { activities = events }
                if case let .tools(tools) = try await ipc.request(.listTools) {
                    toolCatalog = ToolCatalog(tools.map(ToolFacts.init(_:)))
                }
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
            let token = try String(contentsOf: tokenURL, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            _ = try await ipc.request(request(token))
            await refresh()
        } catch { lastError = error.localizedDescription }
    }

    /// Tools of one server, for its detail view.
    func tools(for server: String) -> [ToolFacts] { toolCatalog.tools(for: server) }

    func setToolEnabled(_ tool: String, _ enabled: Bool) async {
        await perform { .setToolEnabled(authToken: $0, tool: tool, enabled: enabled) }
    }

    func updateServer(name: String, config: ServerConfig) async {
        await perform { .updateServer(authToken: $0, name: name, server: config) }
    }

    /// Which AI apps are wired into Plug. Read from the client configuration
    /// files on disk rather than the daemon, which does not own them.
    func loadConnectableApps() async {
        do { connectableApps = try await appLinker.apps() } catch {
            lastError = error.localizedDescription
        }
    }

    func setAppLinked(_ target: String, _ linked: Bool) async {
        guard busyApps.insert(target).inserted else { return }
        defer { busyApps.remove(target) }
        do {
            if linked {
                try await appLinker.link(target: target)
            } else {
                try await appLinker.unlink(target: target)
            }
            await loadConnectableApps()
            await refresh()
        } catch { lastError = error.localizedDescription }
    }

    /// Forgets a server's stored account. The button that starts this is behind
    /// a confirmation, so by the time it runs the choice has been made.
    func signOut(server: String) async {
        do {
            try await AuthFlowService().signOut(server: server)
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
