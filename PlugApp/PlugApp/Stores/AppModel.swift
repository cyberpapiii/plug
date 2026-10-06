import Foundation
import Observation
import PlugIPC

@MainActor
protocol InstallationCoordinating: AnyObject {
    var state: InstallationState { get }
    func reconcile(trigger: ReconciliationTrigger) async
    func adopt() async
    func retry() async
    func restartService() async throws
    func stopService() async throws
    func openLog()
}

@MainActor
extension InstallationCoordinator: InstallationCoordinating {}

@MainActor @Observable
final class AppModel {
    /// `reconnecting` is a working connection that just dropped. A daemon swap
    /// looks exactly like that for about a second, so it is not called
    /// stopped until the drop outlasts `reconnectGrace`.
    enum ConnectionState: Equatable { case disconnected, connecting, reconnecting, incompatible, ready }

    static let defaultClientVersion = PlugIPCClient.clientVersion(
        from: Bundle.main.infoDictionary ?? [:]
    )

    struct ServerPresentation: Identifiable, Equatable {
        let configured: ConfiguredServer
        let runtime: ServerStatus?
        var daemonUptimeSecs: UInt64 = 0
        var id: String { configured.name }
        var health: ServerHealth {
            ServerHealth(
                daemonValue: configured.enabled ? runtime?.health : "Disabled",
                enabled: configured.enabled,
                daemonUptimeSecs: daemonUptimeSecs
            )
        }
        var toolCount: Int { runtime?.toolCount ?? 0 }
    }

    private let ipc: PlugIPCClient
    private let coordinator: any InstallationCoordinating
    private let appLinker: any AppLinking
    private let tokenURL: URL
    private let clientVersion: String
    private let serviceDisabledURL: URL
    private(set) var serviceEnabled: Bool
    private(set) var isChangingService = false
    static var defaultServiceDisabledURL: URL {
        PlugIPCClient.defaultSocketURL.deletingLastPathComponent().appending(path: "service-disabled")
    }
    private var monitoringTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    private var refreshRequestedAgain = false
    private var refreshAgainForcesCatalog = false
    private var reconciliationTask: Task<Void, Never>?
    private var hasStarted = false
    private var reconciliationInFlight = false
    private var attemptedSkewRecovery = false
    /// Not `.disconnected`: before the first handshake nobody has asked, and
    /// "Plug is not running" is an answer, not a question.
    private(set) var connectionState: ConnectionState = .connecting
    private(set) var snapshot: OperatorSnapshot = .empty
    private(set) var hasLoadedSnapshot = false
    /// Called with each snapshot read. The app uses it to find icons; a
    /// model under test leaves it unset and asks the daemon nothing extra.
    @ObservationIgnored var snapshotDidLoad: ((OperatorSnapshot) -> Void)?
    private(set) var activities: [ActivityEvent] = []
    /// How far back the history goes. The daemon keeps a bounded ring, so this
    /// is the whole of what can be asked for, not a page of a longer list.
    static let activityLimit = 200
    /// True only after Plug has actually discarded older rows. Reaching exactly
    /// 200 events is not proof that a 201st event exists.
    private(set) var activityWasTruncated = false
    var activityIsCapped: Bool { activityWasTruncated }
    /// The one line Activity shows about its list: how many calls, how many
    /// of them failed, and whether older ones were dropped. Nil with no calls.
    var activitySummary: String? {
        guard hasLoadedSnapshot, !activities.isEmpty else { return nil }
        let count = activities.count
        let failed = activities.filter { CallFacts($0).failed }.count
        var summary = activityIsCapped
            ? "Latest \(Self.activityLimit) calls"
            : "\(count) recent \(count == 1 ? "call" : "calls")"
        if failed > 0 { summary += ", \(failed) failed" }
        return dataIsStale ? "Last known · \(summary)" : summary
    }
    /// Why the last read of the daemon failed. Polling owns this: the next
    /// good read clears it, and the verdict already says Plug is unreachable.
    private(set) var connectionError: String?
    /// A button press that failed. Kept apart from `connectionError` because a
    /// poll that succeeds says nothing about the press, and clearing one with
    /// the other used to wipe the message before anyone could read it.
    private(set) var actionError: ActionError?
    private(set) var signingInServers: Set<String> = []
    /// The running `plug auth login` per server, so Cancel and Try Again can
    /// stop it instead of leaving it waiting on the browser.
    private var signInTasks: [String: Task<Void, Never>] = [:]
    private(set) var toolCatalog = ToolCatalog()
    private(set) var connectableApps: [LinkableApp] = []
    private(set) var hasLoadedConnectableApps = false
    private(set) var isLoadingConnectableApps = false
    private(set) var connectableAppsError: String?
    private(set) var busyApps: Set<String> = []
    private(set) var busyTools: Set<String> = []
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
        "Restart Plug to finish updating, then edit this server."
    /// Someone is looking at Plug right now, so refresh briskly. Nothing is
    /// visible otherwise, and a background poll every few seconds is rude to
    /// a laptop battery for information no one is reading.
    private var watcherCount = 0

    static let foregroundPollInterval = Duration.seconds(2)
    static let backgroundPollInterval = Duration.seconds(30)
    static let reconnectPollInterval = Duration.seconds(1)
    static let reconnectGrace = Duration.seconds(10)
    private let foregroundPollInterval: Duration
    private let backgroundPollInterval: Duration
    private let reconnectGrace: Duration
    private let authFlow: AuthFlowService
    /// When a working connection first failed, while it is `reconnecting`.
    private var connectionLostAt: ContinuousClock.Instant?

    private var pollInterval: Duration {
        let interval = watcherCount > 0 ? foregroundPollInterval : backgroundPollInterval
        return connectionState == .reconnecting ? min(interval, Self.reconnectPollInterval) : interval
    }

    /// Called when a surface appears or disappears. Balanced pairs only.
    func setWatching(_ watching: Bool) {
        let wasWatched = watcherCount > 0
        watcherCount = max(0, watcherCount + (watching ? 1 : -1))
        guard watching else { return }
        // The poll loop may be halfway through a background sleep. Left alone,
        // a panel opened now would show state up to that long out of date
        // until the sleep ran out, so start the loop over at the new pace.
        if !wasWatched, monitoringTask != nil { startMonitoring() }
        Task { await refresh() }
    }

    init(
        ipc: PlugIPCClient? = nil,
        coordinator: any InstallationCoordinating = InstallationCoordinator(),
        clientVersion: String = AppModel.defaultClientVersion,
        tokenURL: URL = PlugIPCClient.defaultTokenURL,
        appLinker: any AppLinking = AppLinkService(),
        foregroundPollInterval: Duration = AppModel.foregroundPollInterval,
        backgroundPollInterval: Duration = AppModel.backgroundPollInterval,
        reconnectGrace: Duration = AppModel.reconnectGrace,
        authFlow: AuthFlowService = AuthFlowService(),
        serviceDisabledURL: URL = AppModel.defaultServiceDisabledURL
    ) {
        self.clientVersion = clientVersion
        self.ipc = ipc ?? PlugIPCClient(clientVersion: clientVersion)
        self.coordinator = coordinator
        self.tokenURL = tokenURL
        self.appLinker = appLinker
        self.foregroundPollInterval = foregroundPollInterval
        self.backgroundPollInterval = backgroundPollInterval
        self.reconnectGrace = reconnectGrace
        self.authFlow = authFlow
        self.serviceDisabledURL = serviceDisabledURL
        self.serviceEnabled = !FileManager.default.fileExists(atPath: serviceDisabledURL.path)
        if !serviceEnabled { connectionState = .disconnected }
    }

    /// Read live rather than mirrored. A copy refreshed only when a
    /// reconciliation ends reports the state the app was in before the work
    /// started, which is how a repair in progress came to describe itself with
    /// the pre-repair situation.
    private var installationState: InstallationState { coordinator.state }

    /// The installation is being looked at, not changed, so reading the
    /// daemon alongside is safe.
    private var isOnlyInspecting: Bool {
        if case .reconcilingUpdate(.inspecting) = installationState { return true }
        return false
    }

    private var readsAllowed: Bool { serviceEnabled && !isChangingService && (!reconciliationInFlight || isOnlyInspecting) }

    var visibleServers: [ServerPresentation] {
        let runtimeByName = Dictionary(uniqueKeysWithValues: snapshot.servers.map { ($0.serverId, $0) })
        return Self.displayOrder(snapshot.configuredServers.map {
            ServerPresentation(
                configured: $0,
                runtime: runtimeByName[$0.name],
                daemonUptimeSecs: snapshot.uptimeSecs
            )
        })
    }

    /// Servers that are not working come first; each group keeps config order.
    /// It reads the same health the rows show, so a degraded server, which
    /// still routes calls, sorts with the working ones.
    static func displayOrder(_ servers: [ServerPresentation]) -> [ServerPresentation] {
        servers.enumerated().sorted {
            let lhsBad = $0.element.health != .working
            let rhsBad = $1.element.health != .working
            return lhsBad == rhsBad ? $0.offset < $1.offset : lhsBad && !rhsBad
        }.map(\.element)
    }

    /// Everything the interface needs to describe Plug, as one plain value.
    var situation: PlugSituation {
        PlugSituation(
            setup: setupState,
            runtime: runtimeState,
            servers: serverFacts,
            connectedApps: connectedClients.count,
            connectedAppTargets: connectedClients.map(\.target),
            connectedClients: connectedClients,
            version: snapshot.runtimeVersion
        )
    }

    /// A call in the words every surface uses, under the name its owner gave
    /// the client that made it.
    func call(_ event: ActivityEvent) -> CallFacts {
        let name = event.clientKey.flatMap { key in
            snapshot.clientNames?.first { $0.key == key }?.name
        }
        let grant = event.clientKey.flatMap { key in
            snapshot.downstreamClients.first { $0.clientKey == key }?.clientName
        }
        return CallFacts(event, ownerName: name, grantName: grant)
    }

    /// Distinct connected apps, first seen first, so the panel's icon row is
    /// stable while sessions come and go.
    private var connectedClients: [ConnectedClient] {
        ClientNames(snapshot: snapshot).connectedClients(snapshot.liveSessions)
    }

    /// The single sentence every surface renders.
    var verdict: Verdict { PlugVerdict.verdict(for: situation) }

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
                health: server.health,
                toolCount: server.toolCount,
                error: server.runtime?.error,
                isSigningIn: signingInServers.contains(server.configured.name),
                tokenExpiresInSecs: account?.tokenExpiresInSecs,
                authWarnings: account?.warnings ?? []
            )
        }
    }

    private var setupState: PlugSituation.Setup {
        if !serviceEnabled { return .ready }
        switch installationState {
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

    private var runtimeState: PlugSituation.Runtime {
        if !serviceEnabled { return .off }
        if isRestartingService { return .restarting }
        switch connectionState {
        case .ready: return .running
        case .connecting: return .starting
        case .reconnecting: return .reconnecting
        case .incompatible: return .versionMismatch
        case .disconnected: return .stopped
        }
    }

    var menuBarMark: MenuBarMark { PlugVerdict.menuBarMark(for: verdict) }

    /// The version About shows: the daemon's once it has answered, the app's
    /// own before that. The two match except in the middle of an update.
    var displayVersion: String {
        snapshot.runtimeVersion.isEmpty ? clientVersion : snapshot.runtimeVersion
    }

    var isLoadingInitialData: Bool {
        !hasLoadedSnapshot && connectionState == .connecting
    }

    var initialDataUnavailable: Bool {
        !hasLoadedSnapshot && connectionState != .connecting
    }

    var dataIsStale: Bool {
        hasLoadedSnapshot && connectionState != .ready
    }

    var canMutate: Bool { serviceEnabled && !isChangingService && connectionState == .ready }

    /// Recent calls that touched one server, newest first.
    func recentActivity(for server: String, limit: Int = 12) -> [ActivityEvent] {
        activities
            .filter { $0.server == server }
            .sorted { $0.sequence > $1.sequence }
            .prefix(limit)
            .map { $0 }
    }

    func start() async {
        guard !hasStarted else { return }
        hasStarted = true
        guard serviceEnabled else { return }
        // Every launch opens with a pass that only reads the installation,
        // and the daemon is nearly always already running. Waiting for the
        // pass before asking the daemon anything kept a healthy Plug on
        // "Starting…" for seconds.
        let readsDuringLaunch = isOnlyInspecting
        let launch = Task { await reconcile(trigger: .applicationLaunch) }
        if readsDuringLaunch { await refresh() }
        await launch.value
        guard !Task.isCancelled else { return }
        await refresh()
        guard !Task.isCancelled else { return }
        startMonitoring()
    }

    private func startMonitoring() {
        monitoringTask?.cancel()
        monitoringTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let interval = self?.pollInterval else { return }
                try? await Task.sleep(for: interval)
                guard !Task.isCancelled else { return }
                await self?.refresh()
            }
        }
    }

    func reconcile(trigger: ReconciliationTrigger) async {
        guard serviceEnabled, !isChangingService else { return }
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
        if !serviceEnabled { await setServiceEnabled(true); return }
        attemptedSkewRecovery = false
        // Someone pressed Start. Polls no longer flip a stopped Plug to
        // "Starting…", so the press is the one place that says it is trying.
        if connectionState == .disconnected { connectionState = .connecting }
        await retry()
        await refresh()
    }

    /// Restarts the background service inside the reconciliation gate, so
    /// polling stands aside while the daemon is swapped and a Start Plug
    /// pressed meanwhile waits for this swap instead of starting a second one.
    func restartService() async {
        guard serviceEnabled, !isChangingService else { return }
        guard !isRestartingService else { return }
        isRestartingService = true
        // A reconciliation already running would swallow this one; let it
        // finish first so the restart that was asked for still happens.
        if let reconciliationTask { await reconciliationTask.value }
        await runReconciliation { [weak self, coordinator] in
            do {
                try await coordinator.restartService()
            } catch {
                self?.reportActionError("restart Plug", error)
                await coordinator.retry()
            }
        }
        isRestartingService = false
        // The daemon behind the open descriptor is gone.
        await ipc.disconnect()
        await refresh()
    }

    func openLog() {
        coordinator.openLog()
    }

    func setServiceEnabled(_ enabled: Bool) async {
        guard enabled != serviceEnabled, !isChangingService else { return }
        isChangingService = true
        if let reconciliationTask { await reconciliationTask.value }
        if let refreshTask { await refreshTask.value }
        do {
            if enabled {
                try FileManager.default.removeItem(at: serviceDisabledURL)
                serviceEnabled = true
                connectionState = .connecting
                // Turning on is explicit consent to register our own agent.
                await coordinator.adopt()
            } else {
                try FileManager.default.createDirectory(
                    at: serviceDisabledURL.deletingLastPathComponent(), withIntermediateDirectories: true
                )
                try Data().write(to: serviceDisabledURL, options: .atomic)
                do { try await coordinator.stopService() }
                catch {
                    try FileManager.default.removeItem(at: serviceDisabledURL)
                    throw error
                }
                serviceEnabled = false
                monitoringTask?.cancel()
                await ipc.disconnect()
                connectionState = .disconnected
                connectionLostAt = nil
            }
        } catch { reportActionError("turn Plug \(enabled ? "on" : "off")", error) }
        isChangingService = false
        if serviceEnabled {
            await refresh()
            startMonitoring()
        }
    }

    /// Reads the daemon's state. A call that arrives while a read is already
    /// running waits for one more read that starts after it, because the caller
    /// usually just changed something and the read in flight may predate it.
    func refresh(forceCatalog: Bool = false) async {
        guard readsAllowed else { return }
        if let refreshTask {
            refreshRequestedAgain = true
            refreshAgainForcesCatalog = refreshAgainForcesCatalog || forceCatalog
            await refreshTask.value
            return
        }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            var force = forceCatalog
            repeat {
                refreshRequestedAgain = false
                await readDaemonState(forceCatalog: force)
                force = refreshAgainForcesCatalog
                refreshAgainForcesCatalog = false
            } while refreshRequestedAgain && readsAllowed
            refreshRequestedAgain = false
            // Cleared here rather than by the caller, in the same turn that
            // ends the loop, so no request can land between the last read and
            // the task being forgotten.
            refreshTask = nil
        }
        refreshTask = task
        await task.value
    }

    /// No `.connecting` flip on the way in: a poll of a stopped Plug used to
    /// say "Starting…" for the length of each failed attempt, so the headline
    /// flickered between that and "not running" every poll.
    private func readDaemonState(forceCatalog: Bool) async {
        let wasReady = connectionState == .ready
        do {
            do {
                try await readDaemonStateOnce(forceCatalog: forceCatalog)
            } catch _ where wasReady {
                // A working connection that fails was most likely cut by a
                // daemon swap. The client closed the dead descriptor, so one
                // more read reaches whichever daemon is listening now instead
                // of reporting the old one's exit until the next poll.
                try await readDaemonStateOnce(forceCatalog: forceCatalog)
            }
            connectionLostAt = nil
        } catch {
            connectionLost(error)
        }
    }

    /// A drop from a working connection reads as reconnecting, and polls
    /// briskly, until it outlasts the grace. Only then is Plug stopped.
    private func connectionLost(_ error: any Error) {
        connectionError = error.localizedDescription
        guard connectionState == .ready || connectionState == .reconnecting else {
            connectionState = .disconnected
            return
        }
        let now = ContinuousClock.now
        let since = connectionLostAt ?? now
        guard now - since < reconnectGrace else {
            connectionState = .disconnected
            connectionLostAt = nil
            return
        }
        connectionLostAt = since
        guard connectionState != .reconnecting else { return }
        connectionState = .reconnecting
        // The loop may be halfway through a long background sleep.
        if monitoringTask != nil { startMonitoring() }
    }

    private func readDaemonStateOnce(forceCatalog: Bool) async throws {
        let handshake = try await ipc.connect()
        capabilities = Set(handshake.capabilities)
        guard handshake.sharesSupportedIPCVersion else {
            connectionState = .incompatible
            connectionError = nil
            return
        }
        guard handshake.daemonVersion == clientVersion else {
            connectionState = .incompatible
            connectionError = nil
            // During a launch pass the pass itself settles the version, and
            // a retry asked for now would be swallowed by it.
            if !attemptedSkewRecovery, !reconciliationInFlight {
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
        snapshotDidLoad?(value)
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
        connectionError = nil
    }

    func performOperation(_ request: (String) -> IPCRequest) async throws {
        guard canMutate else { throw RuntimeUnavailableError() }
        let token = try String(contentsOf: tokenURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        _ = try await ipc.request(request(token))
        // No forced tool list. Every change an operation can make to it, a
        // switched tool or a rebuilt catalog, moves the snapshot's
        // `tool_catalog_revision`, and the refresh refetches on that alone.
        await refresh()
    }

    /// The operations of the OpenAPI document at `spec`. The daemon may have
    /// to fetch the document, which outlasts the poll's short timeout, so
    /// this asks over a connection of its own.
    func describeAPI(_ spec: String) async throws -> APISummary {
        guard canMutate else { throw RuntimeUnavailableError() }
        let token = try String(contentsOf: tokenURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let patient = PlugIPCClient(clientVersion: clientVersion, requestTimeout: 15)
        guard case let .apiDescribed(api) = try await patient.request(
            .describeAPI(authToken: token, spec: spec)
        ) else {
            throw PlugIPCError.unexpectedResponse("ApiDescribed")
        }
        return api
    }

    /// Runs one change the person asked for. `doing` finishes the sentence
    /// "Could not …", so a failure names what was being attempted.
    func perform(_ doing: String, _ request: (String) -> IPCRequest) async {
        do {
            try await performOperation(request)
        } catch { reportActionError(doing, error) }
    }

    /// Shows a failed press until it is dismissed. A newer failure replaces
    /// it. It does not leave by itself: a message that vanishes while it is
    /// being read is no message.
    private func reportActionError(_ doing: String, _ error: any Error) {
        actionError = ActionError(
            title: "Could not \(doing).",
            message: error.localizedDescription,
            advice: Explain.advice(for: error)
        )
    }

    func dismissActionError() {
        actionError = nil
    }

    /// Tools of one server, for its detail view.
    func tools(for server: String) -> [ToolFacts] { toolCatalog.tools(for: server) }

    func setToolEnabled(_ tool: String, _ enabled: Bool) async {
        guard busyTools.insert(tool).inserted else { return }
        defer { busyTools.remove(tool) }
        await perform("turn \(tool) \(enabled ? "on" : "off")") {
            .setToolEnabled(authToken: $0, tool: tool, enabled: enabled)
        }
    }

    /// Every server with what it said about itself. The snapshot leaves the
    /// icons out, so they are asked for apart from it, and rarely.
    func serverDescriptions() async throws -> [ServerStatus] {
        guard case let .status(servers) = try await ipc.request(.status) else {
            throw PlugIPCError.unexpectedResponse("Status")
        }
        return servers
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
        guard !isLoadingConnectableApps else { return }
        isLoadingConnectableApps = true
        defer {
            isLoadingConnectableApps = false
            hasLoadedConnectableApps = true
        }
        do {
            // Assigned only when it differs: the list is read every few
            // seconds while shown, and an equal value should redraw nothing.
            let apps = try await appLinker.apps()
            if apps != connectableApps { connectableApps = apps }
            if connectableAppsError != nil { connectableAppsError = nil }
        } catch {
            connectableAppsError = error.localizedDescription
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
        } catch {
            let name = AppIcons.displayName(forTarget: target) ?? target
            reportActionError(linked ? "add Plug to \(name)" : "remove Plug from \(name)", error)
        }
    }

    /// Forgets a server's stored account. The button that starts this is behind
    /// a confirmation, so by the time it runs the choice has been made.
    func signOut(server: String) async {
        do {
            try await authFlow.signOut(server: server)
            await refresh()
        } catch { reportActionError("sign out of \(server)", error) }
    }

    /// Starts a sign-in. Pressed again while one is open, it is Try Again:
    /// the open attempt holds the browser callback, so it is stopped and
    /// waited for before the new one starts.
    func signIn(server: String) async {
        if let running = signInTasks[server] {
            running.cancel()
            await running.value
            // A second press that waited on the same attempt has already
            // started the replacement; one browser tab is enough.
            if let current = signInTasks[server], current != running { return }
        }
        let task = Task { [weak self] in _ = await self?.runSignIn(server: server) }
        signInTasks[server] = task
        await task.value
        if signInTasks[server] == task { signInTasks[server] = nil }
    }

    /// Stops an open sign-in. The browser tab stays, but nothing is waiting
    /// on it any more, and the server goes back to offering Sign In.
    func cancelSignIn(server: String) {
        signInTasks[server]?.cancel()
    }

    private func runSignIn(server: String) async {
        signingInServers.insert(server)
        defer { signingInServers.remove(server) }
        do {
            try await authFlow.signIn(server: server)
            await refresh()
        } catch let error where Task.isCancelled || error is CancellationError {
            // Cancelled on purpose. Nothing failed.
        } catch {
            reportActionError("sign in to \(server)", error)
        }
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

/// A press that failed: what was being done, why it failed in the error's
/// own words, and what to do next.
struct ActionError: Identifiable, Equatable {
    let id = UUID()
    let title: String
    let message: String
    let advice: String
}

private struct ServerConfigReadRequiredError: LocalizedError {
    var errorDescription: String? { AppModel.serverConfigReadRequiredCopy }
}

private struct RuntimeUnavailableError: LocalizedError {
    var errorDescription: String? {
        "Plug is starting. Try again in a moment."
    }
}
