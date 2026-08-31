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
    private var hasStarted = false
    private var reconciliationInFlight = false
    private var attemptedSkewRecovery = false
    /// Not `.disconnected`: before the first handshake nobody has asked, and
    /// "Plug is not running" is an answer, not a question.
    private(set) var connectionState: ConnectionState = .connecting
    private(set) var snapshot: OperatorSnapshot = .empty
    private(set) var hasLoadedSnapshot = false
    private(set) var activities: [ActivityEvent] = []
    /// How far back the history goes. The daemon keeps a bounded ring, so this
    /// is the whole of what can be asked for, not a page of a longer list.
    static let activityLimit = 200
    /// True only after Plug has actually discarded older rows. Reaching exactly
    /// 200 events is not proof that a 201st event exists.
    private(set) var activityWasTruncated = false
    var activityIsCapped: Bool { activityWasTruncated }
    private(set) var lastError: String?
    private(set) var signingInServers: Set<String> = []
    private(set) var toolCatalog = ToolCatalog()
    private(set) var connectableApps: [LinkableApp] = []
    private(set) var hasLoadedConnectableApps = false
    private(set) var busyApps: Set<String> = []
    private(set) var isRestartingService = false
    private var capabilities: Set<String> = []
    private var toolCatalogRevision: UInt64?

    /// The daemon accepts per-tool switches. Older daemons do not, and the
    /// interface hides the switches rather than offering a button that fails.
    var canManageTools: Bool { capabilities.contains("tool_mutation") }
    /// The daemon can return one complete server definition. Older daemons
    /// cannot, and Edit Server must not send GetServerConfig until they can.
    var canReadServerConfig: Bool { capabilities.contains("server_config_read") }
    /// Same restart/update sentence Edit Server shows when that capability is
    /// missing, so a Save that never fires is not mistaken for a parse error.
    nonisolated static let serverConfigReadRequiredCopy =
        "Restart required to finish update. The app and its background service are running different versions."
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
    }

    /// Read live rather than mirrored. A copy refreshed only when a
    /// reconciliation ends reports the state the app was in before the work
    /// started, which is how a repair in progress came to describe itself with
    /// the pre-repair situation.
    private var installationState: InstallationState { coordinator.state }

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
            connectedApps: Set(snapshot.liveSessions.map {
                AppIcons.target(forClientType: $0.clientType)
            }).count,
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
        switch installationState {
        case .healthy: return .ready
        case .adoptionRequired: return .needsPermission
        // The first pass only reads the installation, and every launch runs it.
        // Nothing is being set up there, so setup keeps quiet and the runtime
        // verdict says the true thing: Plug is starting. The later phases do
        // change the installation, and those are worth a word.
        case let .reconcilingUpdate(phase): return phase == .inspecting ? .ready : .settingUp
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

    var isLoadingInitialData: Bool {
        !hasLoadedSnapshot && connectionState == .connecting
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
                let interval = self?.pollInterval ?? Self.backgroundPollInterval
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

    func refresh(forceCatalog: Bool = false) async {
        guard refreshTask == nil, !reconciliationInFlight else { return }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            if connectionState != .ready { connectionState = .connecting }
            do {
                let handshake = try await ipc.connect()
                capabilities = Set(handshake.capabilities)
                guard handshake.ipcMin <= 6, handshake.ipcMax >= 3 else {
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
                let daemonRestarted = snapshot.uptimeSecs > 0 && value.uptimeSecs < snapshot.uptimeSecs
                let activityCursor = daemonRestarted ? 0 : (activities.last?.sequence ?? 0)
                snapshot = value
                hasLoadedSnapshot = true
                NotificationService.shared.observe(value)
                if case let .activity(events) = try await ipc.request(
                    .activity(
                        authToken: token,
                        afterSequence: activityCursor,
                        limit: Self.activityLimit + 1,
                        failuresOnly: false
                    )
                ) {
                    if activityCursor == 0 {
                        activityWasTruncated = events.count > Self.activityLimit
                        activities = Array(events.suffix(Self.activityLimit))
                    } else if !events.isEmpty {
                        let merged = activities + events
                        activityWasTruncated = activityWasTruncated
                            || merged.count > Self.activityLimit
                        activities = Array(merged.suffix(Self.activityLimit))
                    }
                }
                // The tool list is nearly a megabyte and the snapshot above
                // already reports when it would answer differently, so ask for
                // it only then. This used to refetch on a timer as well,
                // because the fingerprint was assembled here from server
                // fields and could not see a tool disabled from the CLI. The
                // daemon reports that now.
                let revision = value.toolCatalogRevision
                if forceCatalog || toolCatalog.isEmpty || revision != toolCatalogRevision,
                   case let .tools(tools) = try await ipc.request(.listTools)
                {
                    toolCatalog = ToolCatalog(tools.map(ToolFacts.init(_:)))
                    toolCatalogRevision = revision
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
            await refresh(forceCatalog: true)
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

    func serverConfig(name: String) async throws -> ServerConfig {
        guard canReadServerConfig else {
            throw ServerConfigReadRequiredError()
        }
        let token = try String(contentsOf: tokenURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard case let .serverConfig(returnedName, config) = try await ipc.request(
            .serverConfig(authToken: token, name: name)
        ), returnedName == name else {
            throw PlugIPCError.unexpectedResponse("ServerConfig")
        }
        return config
    }

    /// Which AI apps are wired into Plug. Read from the client configuration
    /// files on disk rather than the daemon, which does not own them.
    func loadConnectableApps() async {
        defer { hasLoadedConnectableApps = true }
        do { connectableApps = try await appLinker.apps() } catch {
            lastError = error.localizedDescription
        }
    }

    func beginServiceRestart() -> Bool {
        guard !isRestartingService else { return false }
        isRestartingService = true
        return true
    }

    func finishServiceRestart(error: (any Error)? = nil) {
        isRestartingService = false
        if let error { lastError = error.localizedDescription }
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
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            await operation()
            self.reconciliationInFlight = false
            self.reconciliationTask = nil
        }
        reconciliationTask = task
        await task.value
    }
}

private struct ServerConfigReadRequiredError: LocalizedError {
    var errorDescription: String? { AppModel.serverConfigReadRequiredCopy }
}
