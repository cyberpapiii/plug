import PlugIPC
import SwiftUI

/// The apps on this Mac, split by the one thing a person checks first: is it
/// talking to Plug right now. Pure value so the grouping can be tested.
struct AppRoster: Equatable {
    struct Connected: Identifiable, Equatable {
        var id: String { app.target }
        let app: LinkableApp
        let sessions: [LiveSession]
    }

    /// Apps with an open session, each with the sessions that belong to it.
    let connected: [Connected]
    /// Installed or configured apps with nothing open.
    let idle: [LinkableApp]
    /// Open sessions from something Plug has no app entry for.
    let other: [LiveSession]

    init(apps: [LinkableApp], sessions: [LiveSession]) {
        var claimed = Set<String>()
        var connected: [Connected] = []
        var idle: [LinkableApp] = []
        for app in apps {
            let own = sessions.filter { Self.targets(of: $0).contains(app.target) }
            claimed.formUnion(own.map(\.sessionId))
            if !own.isEmpty {
                connected.append(Connected(app: app, sessions: own))
            } else {
                idle.append(app)
            }
        }
        self.connected = connected
        self.idle = idle
        self.other = sessions.filter { !claimed.contains($0.sessionId) }
    }

    static func targets(of session: LiveSession) -> Set<String> {
        var targets = [AppIcons.target(forClientType: session.clientType)]
        if let info = session.clientInfo, !info.isEmpty {
            targets.append(AppIcons.target(forClientType: info))
        }
        return Set(targets)
    }
}

extension LiveSession {
    /// The product name of a client Plug knows, nil for any other.
    var knownName: String? {
        for value in [clientType, clientInfo].compactMap({ $0 }) {
            let target = AppIcons.target(forClientType: value)
            if let canonical = AppIcons.displayName(forTarget: target) {
                return canonical
            }
        }
        return nil
    }

    /// What to call a session, best witness first: a client Plug knows, then
    /// the program that started the connector, then whatever the client said
    /// about itself. Pure, so the order can be tested.
    var displayName: String {
        if let knownName { return knownName }
        let type = clientType
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
        let isUnknown = type.localizedCaseInsensitiveCompare("unknown") == .orderedSame
        // An app bundle is a name a person recognises; `python3` is not, so a
        // command line host only wins over a client that said nothing useful.
        if isUnknown, let host, host.app != nil, !host.name.isEmpty {
            return host.name
        }
        if let info = clientInfo,
           !info.isEmpty,
           info.localizedCaseInsensitiveCompare("mcp") != .orderedSame
        {
            return info
        }
        guard isUnknown else { return type }
        if let host, !host.name.isEmpty { return host.name }
        let remote = ["http", "streamable_http", "sse"].contains(transport.lowercased())
        return "Unidentified \(remote ? "remote" : "local") client \(sessionId.prefix(4))"
    }
}

/// The names the owner gave clients, and which client each session is.
///
/// A name belongs to a client, not a session, so it is looked up through the
/// key the daemon stores it under. Pure value so the lookup can be tested.
struct ClientNames: Equatable {
    private let keys: [String: String]
    private let names: [String: String]
    /// The name each remote client registered its grant under.
    private let grantNames: [String: String]

    init(visibility: [ClientVisibility], names: [ClientName], grants: [DownstreamClient] = []) {
        keys = Dictionary(
            visibility.compactMap { entry in entry.clientKey.map { (entry.sessionId, $0) } },
            uniquingKeysWith: { first, _ in first }
        )
        self.names = Dictionary(names.map { ($0.key, $0.name) }, uniquingKeysWith: { first, _ in first })
        grantNames = Dictionary(
            grants.filter { !$0.clientName.isEmpty }.map { ($0.clientKey, $0.clientName) },
            uniquingKeysWith: { first, _ in first }
        )
    }

    init(snapshot: OperatorSnapshot) {
        self.init(
            visibility: snapshot.clientVisibility,
            names: snapshot.clientNames ?? [],
            grants: snapshot.downstreamClients
        )
    }

    /// Nil for a session the daemon cannot tell apart from other clients.
    /// There is nothing to store a name under, so it cannot be renamed.
    func key(of session: LiveSession) -> String? { keys[session.sessionId] }

    func name(forKey key: String?) -> String? { key.flatMap { names[$0] } }

    /// The owner's name first, then a product Plug knows. A remote session
    /// Plug does not know takes the name on its grant, which the owner
    /// approved, ahead of whatever the session says about itself.
    func displayName(_ session: LiveSession) -> String {
        let key = key(of: session)
        return name(forKey: key)
            ?? session.knownName
            // A link says which client it was written for; that outranks
            // the program that happened to start the connector.
            ?? key.flatMap(AppIcons.displayName(forTarget:))
            ?? key.flatMap { grantNames[$0] }
            ?? session.displayName
    }
}

/// What one client may reach, as its row offers it. A block belongs to a
/// client, so it is looked up through the key its requests carry. Pure value
/// so the lookup can be tested.
struct ClientAccess: Equatable {
    let key: String
    let name: String
    /// A remote client is held to its blocks. One on this Mac is not: it can
    /// connect under another name.
    let isRemote: Bool
    let servers: [ConfiguredServer]
    let blockedServers: Set<String>
    /// Single tools the owner blocked from the command line.
    let blockedToolCount: Int

    init(key: String, name: String, servers: [ConfiguredServer], blocks: [ClientBlocks]) {
        self.key = key
        self.name = name
        isRemote = key.hasPrefix("oauth:")
        self.servers = servers
        let blocks = blocks.first { $0.key == key }
        // A block on a server that is gone does nothing and is not counted.
        blockedServers = Set(blocks?.servers ?? []).intersection(servers.map(\.name))
        blockedToolCount = blocks?.tools?.count ?? 0
    }

    var isLimited: Bool { !blockedServers.isEmpty || blockedToolCount > 0 }

    /// What the row says when the client is kept from something.
    var summary: String? {
        guard isLimited else { return nil }
        var parts: [String] = []
        if !blockedServers.isEmpty {
            let count = blockedServers.count
            parts.append(count == 1 ? "1 server" : "\(count) servers")
        }
        if blockedToolCount > 0 {
            parts.append(blockedToolCount == 1 ? "1 tool" : "\(blockedToolCount) tools")
        }
        return "kept from " + parts.joined(separator: " and ")
    }
}

/// Who can use Plug. Every client is one row with one switch: on, it can use
/// Plug; off, it cannot. Everything else about a client, its servers, its
/// name, what it has open, is beside the list when the row is selected, so
/// the list answers "who reaches my tools, and how do I cut them off?" at a
/// glance.
struct ClientsView: View {
    let model: AppModel
    @Bindable var router: Router
    @Binding var search: String
    let run: (PlugIntent) -> Void

    /// Keep rarely used legacy integrations out of the default inventory. If
    /// one is still linked or live, it remains visible so status is never
    /// hidden from the person who needs to act on it.
    private static let secondaryTargets: Set<String> = ["roocode", "goose"]

    private var sessions: [LiveSession] { model.snapshot.liveSessions }
    private var allApps: [LinkableApp] {
        model.connectableApps.filter { app in
            guard app.detected || app.linked || app.live else { return false }
            let isSecondary = Self.secondaryTargets.contains(app.target.lowercased())
            return !isSecondary || app.linked || app.live
        }
    }
    private var apps: [LinkableApp] {
        allApps.filter { matches($0.name) || matches($0.target) }
    }
    /// Every app is placed against all sessions, then search narrows what
    /// shows, so a session never moves to "Other" because its app was
    /// filtered out.
    private var roster: AppRoster { AppRoster(apps: allApps, sessions: sessions) }
    private var connectedApps: [AppRoster.Connected] {
        roster.connected.filter { matches($0.app.name) || matches($0.app.target) }
    }
    private var idleApps: [LinkableApp] {
        roster.idle.filter { matches($0.name) || matches($0.target) }
    }
    private var names: ClientNames { ClientNames(snapshot: model.snapshot) }
    private var grants: [DownstreamClient] {
        model.snapshot.downstreamClients.filter {
            matches(names.name(forKey: $0.clientKey) ?? "") || matches($0.clientName)
                || matches($0.source) || matches($0.clientId)
        }
    }
    private var unmatchedSessions: [LiveSession] {
        roster.other.filter {
            matches(displayName($0)) || matches($0.transport) || matches($0.sessionId)
        }
    }
    @State private var renaming: Renaming?
    @State private var newName = ""
    /// The network client being turned off, while the app asks first.
    @State private var revoking: Entry?

    /// The client being renamed.
    private struct Renaming {
        let key: String
        /// Set when the owner already named it, so the name can be taken back.
        let hasName: Bool
    }

    private func access(key: String, name: String) -> ClientAccess {
        ClientAccess(
            key: key,
            name: name,
            servers: model.snapshot.configuredServers,
            blocks: model.snapshot.clientBlocks ?? []
        )
    }

    /// Server choices for an app on this Mac, stored under the name its link
    /// was written for. An app that reaches Plug over the network sends its
    /// requests under its grant instead, and is offered them in its row under
    /// Over the network; a choice made here would not reach it.
    private func access(to app: LinkableApp, sessions: [LiveSession]) -> ClientAccess? {
        guard app.linked || !sessions.isEmpty else { return nil }
        let overNetwork = app.transport?.lowercased() == "http"
            || sessions.contains { accessKey(of: $0) != app.target }
        guard !overNetwork else { return nil }
        return access(key: app.target, name: names.name(forKey: app.target) ?? app.name)
    }

    /// The key a session's requests carry, when it is one a block can be
    /// stored under. A remote session with no grant shares its key with every
    /// other such session, so it is not offered server choices of its own.
    private func accessKey(of session: LiveSession) -> String? {
        guard let key = names.key(of: session) else { return nil }
        let remote = ["http", "streamable_http", "sse"].contains(session.transport.lowercased())
        return remote && !key.hasPrefix("oauth:") ? nil : key
    }

    private func rename(key: String, shown: String) {
        newName = shown
        renaming = Renaming(key: key, hasName: names.name(forKey: key) != nil)
    }

    var body: some View {
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading clients…")
            } else if model.initialDataUnavailable {
                UnavailablePage(item: "Clients") { run(.reconnect) }
            } else if model.isLoadingConnectableApps && entries.isEmpty {
                LoadingPage(message: "Looking for clients…")
            } else if entries.isEmpty {
                if let error = model.connectableAppsError {
                    ContentUnavailableView {
                        Label("Plug could not look for clients", systemImage: "exclamationmark.triangle")
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try Again") { Task { await model.loadConnectableApps() } }
                    }
                } else if search.trimmingCharacters(in: .whitespaces).isEmpty {
                    EmptyPage(
                        title: "No clients yet",
                        message: "A client is an app that uses your servers, such as Claude or Cursor. When Plug finds one on this Mac, it shows here with a switch.",
                        symbol: "macwindow.on.rectangle",
                        actionTitle: "How Plug works",
                        actionIntent: .showGuide,
                        run: run
                    )
                } else {
                    ContentUnavailableView.search(text: search)
                }
            } else {
                ListDetail {
                    List(selection: $router.selectedClient) {
                        group(.connected)
                        group(.onThisMac)
                        group(.network)
                    }
                } detail: {
                    if let selected {
                        ClientDetail(
                            entry: selected,
                            rename: selected.renameKey.map { key in { rename(key: key, shown: selected.name) } },
                            run: run
                        )
                        .id(selected.id)
                    } else {
                        NoSelection(item: "Client")
                    }
                }
            }
        }
        .navigationSubtitle(connectionSummary ?? "")
        .onChange(of: entries.map(\.id), initial: true) { keepSelectionVisible() }
        .confirmationDialog(
            "Turn off \(revoking?.name ?? "")?",
            isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            titleVisibility: .visible,
            presenting: revoking
        ) { entry in
            Button("Turn Off", role: .destructive) {
                if let id = entry.grantID { run(.revokeClient(id: id)) }
            }
            Button("Cancel", role: .cancel) { }
        } message: { _ in
            Text("It stops being able to use Plug right away, and leaves this list. To come back it has to ask you again.")
        }
        .task { await model.loadConnectableApps() }
        .alert(
            "Rename Client",
            isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } }),
            presenting: renaming
        ) { target in
            TextField("Name", text: $newName)
            Button("Rename") { run(.renameClient(key: target.key, name: newName)) }
            if target.hasName {
                Button("Use Original Name") { run(.renameClient(key: target.key, name: "")) }
            }
            Button("Cancel", role: .cancel) { }
        } message: { _ in
            Text("Only Plug shows this name. The client itself is not changed.")
        }
    }

    private var selected: Entry? { entries.first { $0.id == router.selectedClient } }

    /// NSTableView is still finishing its own update when the list changes, so
    /// the selection moves on the next main-actor turn.
    private func keepSelectionVisible() {
        let ids = entries.map(\.id)
        if let current = router.selectedClient, ids.contains(current) { return }
        Task { @MainActor in
            await Task.yield()
            router.selectedClient = ids.first
        }
    }

    @ViewBuilder
    private func group(_ group: Entry.Group) -> some View {
        let rows = entries.filter { $0.group == group }
        if !rows.isEmpty {
            Section(group.rawValue) {
                ForEach(rows) { entry in
                    ClientRow(entry: entry).tag(entry.id)
                }
            }
        }
    }

    // MARK: - Entries

    /// Every client the list shows, in the order it shows them. The list and
    /// the detail beside it both read from here, so they cannot disagree.
    private var entries: [Entry] {
        connectedApps.map { appEntry($0.app, sessions: $0.sessions) }
            + unmatchedSessions.map(sessionEntry)
            + idleApps.map { appEntry($0, sessions: []) }
            + grants.map(grantEntry)
    }

    /// A client on this Mac. Its switch adds Plug to the client's settings or
    /// takes it out.
    private func appEntry(_ app: LinkableApp, sessions: [LiveSession]) -> Entry {
        let name = names.name(forKey: app.target) ?? app.name
        let access = access(to: app, sessions: sessions)
        let known = app.detected || app.linked
        return Entry(
            id: "app:\(app.target)",
            group: sessions.isEmpty ? .onThisMac : .connected,
            name: name,
            status: ClientStatus.app(app, connections: sessions.count, limit: access?.summary),
            isLive: !sessions.isEmpty,
            dimmed: !known && sessions.isEmpty,
            glyph: .app(target: app.target, name: app.name, appPath: nil),
            isBusy: model.busyApps.contains(app.target),
            switchLabel: "Use Plug",
            isOn: known ? app.linked : nil,
            setOn: { run($0 ? .linkApp(app.target) : .unlinkApp(app.target)) },
            about: app.linked
                ? "Plug is in this client's settings. Restart the client after you change this."
                : "Plug is not in this client's settings. Turn it on to add it, then restart the client.",
            access: access,
            connections: sessions.map(connection),
            renameKey: access?.key
        )
    }

    /// Something connected that Plug has no client entry for. There is nothing
    /// to switch: it is here because it is connected.
    private func sessionEntry(_ session: LiveSession) -> Entry {
        let name = displayName(session)
        return Entry(
            id: "session:\(session.sessionId)",
            group: .connected,
            name: name,
            status: ClientStatus(text: connectionDescription(session), symbol: "bolt.fill"),
            isLive: true,
            dimmed: false,
            glyph: .app(target: sessionTarget(session), name: name, appPath: session.host?.app),
            isBusy: false,
            switchLabel: "",
            isOn: nil,
            setOn: { _ in },
            about: "Plug does not know this client by name, so it has no switch. It shows here while it is connected.",
            access: accessKey(of: session).map { access(key: $0, name: name) },
            connections: [connection(session)],
            renameKey: names.key(of: session)
        )
    }

    /// A client allowed in over the network. Its switch is its permission:
    /// off takes the permission away, and the client has to ask again, so the
    /// app asks first.
    private func grantEntry(_ grant: DownstreamClient) -> Entry {
        let name = names.name(forKey: grant.clientKey) ?? grant.clientName
        let access = access(key: grant.clientKey, name: name)
        let host = URL(string: grant.clientId)?.host()
        let origin = host ?? "Allowed"
        // A client Plug registered gets a short id, since two can share a
        // name. It is in the detail, not the row, for the rare time two need
        // telling apart.
        let short = grant.clientId.hasPrefix("plug_")
            ? grant.clientId.dropFirst(5) : Substring(grant.clientId)
        let id = "grant:\(grant.clientId)"
        return Entry(
            id: id,
            group: .network,
            name: name,
            status: ClientStatus(
                text: access.summary.map { "\(origin) · \($0)" } ?? origin,
                symbol: "network"
            ),
            isLive: false,
            dimmed: false,
            glyph: .grant,
            isBusy: false,
            switchLabel: "Allowed",
            isOn: true,
            setOn: { allowed in
                if !allowed { revoking = entries.first { $0.id == id } }
            },
            about: "You allowed this client to use Plug over the network. Turn it off to take that back.",
            access: access,
            connections: [],
            renameKey: grant.clientKey,
            identity: host.map { "From \($0)" } ?? "ID \(short.prefix(8))",
            grantID: grant.clientId
        )
    }

    private func connection(_ session: LiveSession) -> Entry.Connection {
        Entry.Connection(
            id: session.sessionId,
            how: connectionDescription(session),
            tools: toolsText(session)
        )
    }

    private func matches(_ value: String) -> Bool {
        let query = search.trimmingCharacters(in: .whitespaces)
        return query.isEmpty || value.localizedCaseInsensitiveContains(query)
    }

    private var connectionSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = roster.connected.count + roster.other.count
        let summary = count == 1 ? "1 client connected" : "\(count) clients connected"
        return model.dataIsStale ? "Last known · \(summary)" : summary
    }

    private func displayName(_ session: LiveSession) -> String { names.displayName(session) }

    /// The target whose icon a session shows: the client it reports, else the
    /// client its link was written for.
    private func sessionTarget(_ session: LiveSession) -> String {
        let reported = AppIcons.target(forClientType: session.clientType)
        if AppIcons.displayName(forTarget: reported) != nil { return reported }
        if let key = names.key(of: session), AppIcons.displayName(forTarget: key) != nil {
            return key
        }
        return reported
    }

    /// Says how it reached Plug in words, not transport identifiers.
    private func connectionDescription(_ session: LiveSession) -> String {
        let how: String
        switch session.transport.lowercased() {
        case "stdio", "ipc", "daemon_proxy": how = "On this Mac"
        case "http", "streamable_http", "sse": how = "Over the network"
        default: how = session.transport.replacingOccurrences(of: "_", with: " ").capitalized
        }
        return "\(how) · \(duration(session.connectedSecs))"
    }

    private func duration(_ seconds: UInt64) -> String {
        if seconds < 60 { return "just connected" }
        if seconds < 3_600 { return "connected \(seconds / 60)m" }
        if seconds < 86_400 { return "connected \(seconds / 3_600)h" }
        return "connected \(seconds / 86_400)d"
    }

    private func toolsText(_ session: LiveSession) -> String {
        guard let count = model.snapshot.clientVisibility
            .first(where: { $0.sessionId == session.sessionId })?
            .visibleToolCount
        else { return "" }
        return count == 1 ? "1 tool" : "\(count) tools"
    }
}

/// The one line under a client's name, with a glyph so the state is told
/// before the sentence is read. Pure value so the wording can be tested.
struct ClientStatus: Equatable {
    let text: String
    let symbol: String

    init(text: String, symbol: String) {
        self.text = text
        self.symbol = symbol
    }

    /// A client on this Mac: connected, ready, or not using Plug.
    static func app(_ app: LinkableApp, connections: Int, limit: String?) -> ClientStatus {
        let state: String
        let symbol: String
        if connections > 0 {
            state = connections == 1 ? "Connected" : "Connected · \(connections) connections"
            symbol = "bolt.fill"
        } else if !app.linked {
            state = app.detected ? "Off" : "Not installed"
            symbol = app.detected ? "circle" : "questionmark.app.dashed"
        } else if !app.detected {
            state = "On · client not found on this Mac"
            symbol = "checkmark.circle"
        } else {
            state = app.transport?.lowercased() == "http" ? "On · over the network" : "On · not open right now"
            symbol = "checkmark.circle"
        }
        return ClientStatus(text: limit.map { "\(state) · \($0)" } ?? state, symbol: symbol)
    }
}

/// One client as the list and the detail show it.
struct ClientEntry: Identifiable {
    enum Group: String {
        case connected = "Connected Now"
        case onThisMac = "On This Mac"
        case network = "Over the Network"
    }

    enum Glyph {
        case app(target: String, name: String, appPath: String?)
        case grant
    }

    /// One open connection of a client.
    struct Connection: Identifiable {
        let id: String
        let how: String
        let tools: String
    }

    let id: String
    let group: Group
    let name: String
    let status: ClientStatus
    let isLive: Bool
    let dimmed: Bool
    let glyph: Glyph
    let isBusy: Bool
    let switchLabel: String
    /// Nil when this client has nothing to switch.
    let isOn: Bool?
    let setOn: (Bool) -> Void
    let about: String
    /// Nil when there are no server choices to offer for this client.
    let access: ClientAccess?
    let connections: [Connection]
    /// Nil when Plug has nothing to store a name under.
    let renameKey: String?
    /// A line that identifies the client to someone checking it, such as the
    /// site it came from.
    var identity: String?
    /// Set for a client allowed in over the network.
    var grantID: String?
}

private typealias Entry = ClientEntry

private struct ClientGlyph: View {
    let glyph: ClientEntry.Glyph
    var size: CGFloat = 20

    var body: some View {
        switch glyph {
        case let .app(target, name, appPath):
            AppGlyph(target: target, name: name, appPath: appPath, size: size)
        case .grant:
            Image(systemName: "key.horizontal")
                .font(.system(size: size * 0.62))
                .foregroundStyle(.secondary)
                .frame(width: size, height: size)
                .accessibilityHidden(true)
        }
    }
}

/// One client: its icon, its name, and one switch. The switch works without
/// selecting the row.
private struct ClientRow: View {
    let entry: ClientEntry

    var body: some View {
        HStack(spacing: Metric.tight) {
            ClientGlyph(glyph: entry.glyph)
                .opacity(entry.dimmed ? 0.4 : 1)
            Text(entry.name)
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(entry.dimmed ? .secondary : .primary)
                .accessibilityLabel("\(entry.name), \(entry.status.text)")
            Spacer(minLength: Metric.tight)
            ClientSwitch(entry: entry, size: .mini)
        }
    }
}

private struct ClientSwitch: View {
    let entry: ClientEntry
    var size: ControlSize = .regular

    var body: some View {
        if entry.isBusy {
            ProgressView().controlSize(.small)
        } else if let isOn = entry.isOn {
            Toggle(entry.switchLabel, isOn: Binding(get: { isOn }, set: entry.setOn))
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(size)
                .accessibilityLabel("\(entry.switchLabel), \(entry.name)")
        }
    }
}

/// Everything about one client: its switch, the servers it can use, what it
/// has open, and its name.
private struct ClientDetail: View {
    let entry: ClientEntry
    let rename: (() -> Void)?
    let run: (PlugIntent) -> Void

    var body: some View {
        DetailForm {
            Section {
                DetailHeader(title: entry.name, subtitle: entry.status.text) {
                    ClientGlyph(glyph: entry.glyph, size: 32)
                } controls: {
                    ClientSwitch(entry: entry)
                }
            } footer: {
                Text(entry.about)
            }

            if let access = entry.access {
                Section {
                    if access.servers.isEmpty {
                        Text("No servers yet.").foregroundStyle(.secondary)
                    } else {
                        ForEach(access.servers) { server in
                            Toggle(
                                isOn: Binding(
                                    get: { !access.blockedServers.contains(server.name) },
                                    set: {
                                        run(.setClientServerBlocked(
                                            key: access.key, server: server.name, blocked: !$0
                                        ))
                                    }
                                )
                            ) {
                                Text(server.name)
                                    .foregroundStyle(server.enabled ? .primary : .secondary)
                            }
                        }
                    }
                } header: {
                    Text("Servers It Can Use")
                } footer: {
                    Text(Self.note(for: access))
                }
            }

            if !entry.connections.isEmpty {
                Section("Connected Now") {
                    ForEach(entry.connections) { connection in
                        LabeledContent(connection.how, value: connection.tools)
                            .help("Connection \(connection.id.prefix(8))")
                    }
                }
            }

            if rename != nil || entry.identity != nil {
                Section {
                    if let identity = entry.identity {
                        LabeledContent("Identity") {
                            Text(identity).textSelection(.enabled)
                        }
                    }
                    if let rename {
                        LabeledContent("Name in Plug") {
                            Button("Rename…", action: rename)
                        }
                    }
                }
            }
        }
    }

    static func note(for access: ClientAccess) -> String {
        var lines = [
            access.isRemote
                ? "A client over the network cannot get around this."
                : "This keeps a client's list short. It is not a lock: a client on this Mac can connect under another name.",
        ]
        if access.blockedToolCount > 0 {
            let count = access.blockedToolCount
            lines.append("It is also kept from \(count == 1 ? "1 tool" : "\(count) tools").")
        }
        return lines.joined(separator: " ")
    }
}
