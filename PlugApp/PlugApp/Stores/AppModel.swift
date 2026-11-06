import Foundation
import Observation
import PlugIPC

/// What Plug looks like right now, read from the daemon on a poll, and the
/// one place the interface reads it from. Changes go through `actions`;
/// installation work and the on/off switch go through `gate`.
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
                daemonValue: runtime?.health,
                enabled: configured.enabled,
                daemonUptimeSecs: daemonUptimeSecs
            )
        }
        var toolCount: Int { runtime?.toolCount ?? 0 }
    }

    let gate: ReconciliationGate
    let actions: PlugActions
    private let daemon: DaemonClient
    private var monitoringTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    private var refreshRequestedAgain = false
    private var refreshAgainForcesCatalog = false
    private var hasStarted = false
    private var attemptedSkewRecovery = false
    /// Not `.disconnected`: before the first handshake nobody has asked, and
    /// "Plug is not running" is an answer, not a question.
    private(set) var connectionState: ConnectionState = .connecting
    private(set) var snapshot: OperatorSnapshot = .empty
    private(set) var hasLoadedSnapshot = false
    /// Called with each snapshot read, for what the app keeps beside the
    /// model: notifications and icons. A model under test leaves it unset.
    @ObservationIgnored var snapshotDidLoad: (@MainActor (OperatorSnapshot) -> Void)?
    private(set) var activity = ActivityLog()
    /// Why the last read of the daemon failed. Polling owns this: the next
    /// good read clears it, and the verdict already says Plug is unreachable.
    private(set) var connectionError: String?
    private(set) var toolCatalog = ToolCatalog()
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
    /// When a working connection first failed, while it is `reconnecting`.
    private var connectionLostAt: ContinuousClock.Instant?

    /// `home` is the folder of the daemon this model reads: its socket, its
    /// token and the off marker. Tests pass their own.
    init(
        home: PlugHome,
        ipc: PlugIPCClient? = nil,
        coordinator: (any InstallationCoordinating)? = nil,
        clientVersion: String = AppModel.defaultClientVersion,
        appLinker: any AppLinking = AppLinkService(),
        foregroundPollInterval: Duration = AppModel.foregroundPollInterval,
        backgroundPollInterval: Duration = AppModel.backgroundPollInterval,
        reconnectGrace: Duration = AppModel.reconnectGrace,
        authFlow: AuthFlowService = AuthFlowService()
    ) {
        let serviceDisabled = ServiceDisabledMarker(home: home)
        let daemon = DaemonClient(
            ipc: ipc ?? PlugIPCClient(socketURL: home.socketURL, clientVersion: clientVersion),
            clientVersion: clientVersion,
            tokenURL: home.tokenURL
        )
        self.daemon = daemon
        // Built here and not as the default argument: Swift 6.1 crashes
        // compiling that call in a default argument.
        gate = ReconciliationGate(
            coordinator: coordinator ?? InstallationCoordinator(
                serviceDisabled: serviceDisabled, laterRetryDelay: .seconds(300)
            ),
            marker: serviceDisabled
        )
        self.foregroundPollInterval = foregroundPollInterval
        self.backgroundPollInterval = backgroundPollInterval
        self.reconnectGrace = reconnectGrace
        var canMutate: @MainActor () -> Bool = { false }
        var refresh: @MainActor () async -> Void = {}
        actions = PlugActions(
            daemon: daemon,
            appLinker: appLinker,
            authFlow: authFlow,
            canMutate: { canMutate() },
            refresh: { await refresh() }
        )
        if !gate.serviceEnabled { connectionState = .disconnected }
        canMutate = { [weak self] in self?.canMutate ?? false }
        refresh = { [weak self] in await self?.refresh() }
    }

    var serviceEnabled: Bool { gate.serviceEnabled }
    var activities: [ActivityEvent] { activity.events }
    var activitySummary: String? { hasLoadedSnapshot ? activity.summary(stale: dataIsStale) : nil }

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

    var visibleServers: [ServerPresentation] {
        let runtimeByName = Dictionary(snapshot.servers.map { ($0.serverId, $0) }, uniquingKeysWith: { first, _ in first })
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
    /// A page reads it a dozen times each time it is drawn, and building it
    /// walks every server and every session, so it is kept until one of its
    /// inputs changes.
    var situation: PlugSituation {
        let key = SituationKey(
            revision: snapshotRevision,
            setup: gate.setup,
            runtime: runtime,
            signingIn: actions.signingInServers
        )
        // Read so a view that only ever sees the kept value still follows
        // the snapshot.
        _ = snapshot.runtimeVersion
        if let kept = keptSituation, kept.key == key { return kept.value }
        let value = buildSituation()
        keptSituation = (key, value)
        return value
    }

    private struct SituationKey: Equatable {
        let revision: Int
        let setup: PlugSituation.Setup
        let runtime: PlugSituation.Runtime
        let signingIn: Set<String>
    }

    @ObservationIgnored private var snapshotRevision = 0
    /// The daemon's own count of its uptime at the last read, unrounded.
    @ObservationIgnored private var lastUptimeSecs: UInt64 = 0
    @ObservationIgnored private var keptSituation: (key: SituationKey, value: PlugSituation)?

    private func buildSituation() -> PlugSituation {
        let connectedClients = clientNames.connectedClients(snapshot.liveSessions)
        return PlugSituation(
            setup: gate.setup,
            runtime: runtime,
            servers: serverFacts,
            connectedApps: connectedClients.count,
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
        let place = event.clientKey.flatMap { key in
            snapshot.clientPlaces?.first { $0.key == key }?.place
        }
        return CallFacts(
            event, delimiter: snapshot.prefixDelimiter, ownerName: name, grantName: grant, place: place
        )
    }

    /// The names and places the owner gave clients. The Clients page asks
    /// for it for every session of every row, so it is built once a snapshot.
    var clientNames: ClientNames {
        // Read first, so a view that only ever sees the kept value still
        // follows the snapshot.
        let snapshot = snapshot
        if let kept = keptClientNames, kept.revision == snapshotRevision { return kept.value }
        let value = ClientNames(snapshot: snapshot)
        keptClientNames = (snapshotRevision, value)
        return value
    }

    @ObservationIgnored private var keptClientNames: (revision: Int, value: ClientNames)?

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
                isSigningIn: actions.signingInServers.contains(server.configured.name),
                tokenExpiresInSecs: account?.tokenExpiresInSecs,
                authWarnings: account?.warnings ?? []
            )
        }
    }

    private var runtime: PlugSituation.Runtime {
        if !gate.serviceEnabled { return .off }
        if gate.isRestartingService { return .restarting }
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
        snapshot.runtimeVersion.isEmpty ? daemon.clientVersion : snapshot.runtimeVersion
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

    var canMutate: Bool { gate.serviceEnabled && !gate.isChangingService && connectionState == .ready }

    /// Where this Mac is in the three first-run steps.
    var firstRunGuide: FirstRunGuide {
        FirstRunGuide(
            serverCount: snapshot.configuredServers.count,
            clientCount: actions.connectableApps.filter { $0.linked || $0.live }.count
                + snapshot.downstreamClients.count,
            hasActivity: !activities.isEmpty
        )
    }

    /// Recent calls that touched one server, newest first.
    func recentActivity(for server: String, limit: Int = 12) -> [ActivityEvent] {
        activities
            .filter { $0.server == server }
            .sorted { $0.sequence > $1.sequence }
            .prefix(limit)
            .map { $0 }
    }

    // MARK: Reading the daemon

    /// One complete server definition, for editing.
    func serverConfig(name: String) async throws -> ServerConfig {
        try await daemon.serverConfig(name: name)
    }

    /// What the daemon knows about where servers come from. See
    /// `DaemonClient.origins(configsFor:)`.
    func origins(configsFor names: [String]) async -> [String: ServerOrigin] {
        await daemon.origins(configsFor: names)
    }

    func start() async {
        guard !hasStarted else { return }
        hasStarted = true
        guard gate.serviceEnabled else { return }
        // Every launch opens with a pass that only reads the installation,
        // and the daemon is nearly always already running. Waiting for the
        // pass before asking the daemon anything kept a healthy Plug on
        // "Starting…" for seconds.
        let readsDuringLaunch = gate.isOnlyInspecting
        // A task of the model's own: the surface that asked may go away before
        // launch is over, and Plug must still end up watching the daemon.
        await Task {
            let launch = Task { await gate.reconcile(trigger: .applicationLaunch) }
            if readsDuringLaunch { await refresh() }
            await launch.value
            await refresh()
            startMonitoring()
        }.value
    }

    private func startMonitoring() {
        monitoringTask?.cancel()
        monitoringTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let interval = self?.pollInterval else { return }
                // A tenth of the wait is slack, so the system can wake Plug
                // along with other work instead of for this alone.
                try? await Task.sleep(for: interval, tolerance: interval / 10)
                guard !Task.isCancelled else { return }
                await self?.refresh()
            }
        }
    }

    // MARK: Installation and the service

    func reconcile(trigger: ReconciliationTrigger) async {
        await gate.reconcile(trigger: trigger)
    }

    func adopt() async {
        await gate.reconcile(trigger: .explicitAdoption)
    }

    func retry() async {
        await daemon.disconnect()
        await gate.reconcile(trigger: .retry)
    }

    func retryConnection() async {
        if !gate.serviceEnabled { await setServiceEnabled(true); return }
        attemptedSkewRecovery = false
        // Someone pressed Start. Polls no longer flip a stopped Plug to
        // "Starting…", so the press is the one place that says it is trying.
        if connectionState == .disconnected { connectionState = .connecting }
        await retry()
        await refresh()
    }

    /// Restarts the background service inside the gate, so polling stands
    /// aside while the daemon is swapped and a Start Plug pressed meanwhile
    /// waits for this swap instead of starting a second one.
    func restartService() async {
        guard gate.serviceEnabled, !gate.isChangingService, !gate.isRestartingService else { return }
        await gate.restart { [weak self] in self?.actions.report("restart Plug", $0) }
        // The daemon behind the open descriptor is gone.
        await daemon.disconnect()
        await refresh()
    }

    func openLog() {
        gate.openLog()
    }

    func setServiceEnabled(_ enabled: Bool) async {
        guard enabled != gate.serviceEnabled, !gate.isChangingService else { return }
        do {
            try await gate.setServiceEnabled(enabled) { [self] in
                if let refreshTask { await refreshTask.value }
                if enabled { connectionState = .connecting }
            }
        } catch {
            actions.report("turn Plug \(enabled ? "on" : "off")", error)
        }
        if gate.serviceEnabled {
            await refresh()
            startMonitoring()
        } else {
            monitoringTask?.cancel()
            await daemon.disconnect()
            connectionState = .disconnected
            connectionLostAt = nil
        }
    }

    // MARK: Polling

    /// Reads the daemon's state. A call that arrives while a read is already
    /// running waits for one more read that starts after it, because the caller
    /// usually just changed something and the read in flight may predate it.
    func refresh(forceCatalog: Bool = false) async {
        guard gate.readsAllowed else { return }
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
            } while refreshRequestedAgain && gate.readsAllowed
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
        // A stopped Plug fails every poll the same way. Saying so again
        // would draw every view again for nothing.
        let message = error.localizedDescription
        if connectionError != message { connectionError = message }
        guard connectionState == .ready || connectionState == .reconnecting else {
            if connectionState != .disconnected { connectionState = .disconnected }
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

    /// Nothing is read from a daemon of another version: the app and its
    /// daemon are one build, so a different version is an update half done.
    private func readDaemonStateOnce(forceCatalog: Bool) async throws {
        let handshake = try await daemon.handshake()
        guard handshake.daemonVersion == daemon.clientVersion else {
            if connectionState != .incompatible { connectionState = .incompatible }
            if connectionError != nil { connectionError = nil }
            // During a launch pass the pass itself settles the version, and
            // a retry asked for now would be swallowed by it.
            if !attemptedSkewRecovery, !gate.isInFlight {
                attemptedSkewRecovery = true
                await retry()
            }
            return
        }
        attemptedSkewRecovery = false
        let value = try await daemon.snapshot()
        let daemonRestarted = lastUptimeSecs > 0 && value.uptimeSecs < lastUptimeSecs
        lastUptimeSecs = value.uptimeSecs
        let cursor = activity.cursor(daemonRestarted: daemonRestarted)
        // A read that says nothing new is not published: every view that
        // shows a server, a client or Plug's status would be drawn again,
        // every poll, for a clock that ticked.
        let steady = value.steadied()
        if steady != snapshot {
            snapshotRevision += 1
            snapshot = steady
        }
        if !hasLoadedSnapshot { hasLoadedSnapshot = true }
        snapshotDidLoad?(value)
        if let page = try await daemon.activity(after: cursor, limit: ActivityLog.limit + 1) {
            var next = activity
            next.take(page, after: cursor)
            if next != activity { activity = next }
        }
        // The tool list is nearly a megabyte, and the snapshot reports when
        // it would answer differently, so it is asked for only then.
        let revision = value.toolCatalogRevision
        if forceCatalog || !toolCatalog.isCurrent(at: revision), let tools = try await daemon.tools() {
            let delimiter = value.prefixDelimiter
            toolCatalog = ToolCatalog(tools.map { ToolFacts($0, delimiter: delimiter) }, revision: revision)
        }
        if connectionState != .ready { connectionState = .ready }
        if connectionError != nil { connectionError = nil }
    }
}
