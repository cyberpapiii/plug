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
/// name, what it has open, is one click away in the row's detail, so the list
/// answers "who reaches my tools, and how do I cut them off?" at a glance.
struct ClientsView: View {
    let model: AppModel
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
    /// The row whose detail is open.
    @State private var opened: String?

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
        opened = nil
        newName = shown
        renaming = Renaming(key: key, hasName: names.name(forKey: key) != nil)
    }

    private func isOpen(_ id: String) -> Binding<Bool> {
        Binding(get: { opened == id }, set: { opened = $0 ? id : nil })
    }

    var body: some View {
        VStack(spacing: 0) {
            PageHeader("Clients", detail: connectionSummary)

            Group {
                if model.isLoadingInitialData {
                    LoadingPage(message: "Loading clients…")
                } else if model.initialDataUnavailable {
                    UnavailablePage(item: "Clients") { run(.reconnect) }
                } else if isEmpty {
                    if search.isEmpty, model.connectableAppsError == nil {
                        EmptyPage(
                            title: "No clients yet",
                            message: "A client is an app that uses your servers, such as Claude or Cursor. When Plug finds one on this Mac, it shows here with a switch.",
                            symbol: "app.connected.to.app.below.fill",
                            actionTitle: "How Plug works",
                            actionIntent: .showGuide,
                            run: run
                        )
                    } else if search.isEmpty {
                        EmptyPage(
                            title: "Plug could not look for clients",
                            message: model.connectableAppsError ?? "",
                            symbol: "exclamationmark.triangle"
                        )
                    } else {
                        EmptyPage(
                            title: "No matching clients",
                            message: "No client matches “\(search.trimmingCharacters(in: .whitespaces))”.",
                            symbol: "magnifyingglass"
                        )
                    }
                } else {
                    List {
                        if model.isLoadingConnectableApps && apps.isEmpty {
                            HStack(spacing: Metric.snug) {
                                ProgressView().controlSize(.small)
                                Text("Looking for clients…").foregroundStyle(.secondary)
                            }
                            .listRowSeparator(.hidden)
                        } else if let error = model.connectableAppsError, apps.isEmpty {
                            HStack(spacing: Metric.snug) {
                                Label(error, systemImage: "exclamationmark.triangle")
                                    .font(.callout)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                                    .layoutPriority(1)
                                Spacer(minLength: 0)
                                Button("Try Again") {
                                    Task { await model.loadConnectableApps() }
                                }
                                .controlSize(.small)
                            }
                            .listRowSeparator(.hidden)
                        }
                        if !connectedApps.isEmpty || !unmatchedSessions.isEmpty {
                            sectionLabel(
                                "Connected now",
                                count: connectedApps.count + unmatchedSessions.count
                            )
                            ForEach(connectedApps) { entry in
                                appRow(entry.app, sessions: entry.sessions)
                            }
                            ForEach(unmatchedSessions) { session in
                                sessionRow(session)
                            }
                        }
                        if !idleApps.isEmpty {
                            sectionLabel("On this Mac", count: idleApps.count)
                            ForEach(idleApps) { app in
                                appRow(app, sessions: [])
                            }
                            footnote("Turn a client on to add Plug to its settings. Restart the client to pick up the change.")
                        }
                        if !grants.isEmpty {
                            sectionLabel("Over the network", count: grants.count)
                            ForEach(grants) { grant in
                                grantRow(grant)
                            }
                            footnote("These clients reach Plug over the network. Turn off any you do not recognize.")
                        }
                    }
                    .listStyle(.inset)
                    .frame(maxWidth: Metric.contentMaxWidth)
                    .frame(maxWidth: .infinity)
                }
            }
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

    private var isEmpty: Bool {
        unmatchedSessions.isEmpty && grants.isEmpty && apps.isEmpty
            && !model.isLoadingConnectableApps
    }

    private func sectionLabel(_ title: String, count: Int) -> some View {
        SectionLabel(text: title, trailing: count == 1 ? "1 client" : "\(count) clients")
            .padding(.top, Metric.regular)
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }

    private func footnote(_ text: String) -> some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }

    // MARK: - Rows

    /// A client on this Mac. Its switch adds Plug to the client's settings or
    /// takes it out.
    private func appRow(_ app: LinkableApp, sessions: [LiveSession]) -> some View {
        let name = names.name(forKey: app.target) ?? app.name
        let access = access(to: app, sessions: sessions)
        let known = app.detected || app.linked
        return ClientRow(
            name: name,
            status: ClientStatus.app(app, connections: sessions.count, limit: access?.summary),
            isLive: !sessions.isEmpty,
            dimmed: !known && sessions.isEmpty,
            isOpen: isOpen("app:\(app.target)"),
            isBusy: model.busyApps.contains(app.target),
            switchLabel: "Use Plug",
            isOn: known ? app.linked : nil,
            setOn: { run($0 ? .linkApp(app.target) : .unlinkApp(app.target)) }
        ) {
            AppGlyph(target: app.target, name: app.name)
        } detail: {
            ClientDetail(
                name: name,
                about: app.linked
                    ? "Plug is in this client's settings."
                    : "Plug is not in this client's settings. Turn it on to add it.",
                access: access,
                connections: sessions.map(connection),
                rename: access.map { access in { rename(key: access.key, shown: name) } },
                run: run
            )
        }
        .listRowSeparator(.hidden)
    }

    /// Something connected that Plug has no client entry for. There is nothing
    /// to switch: it is here because it is connected.
    private func sessionRow(_ session: LiveSession) -> some View {
        let name = displayName(session)
        let key = accessKey(of: session)
        return ClientRow(
            name: name,
            status: ClientStatus(
                text: connectionDescription(session),
                symbol: "bolt.fill"
            ),
            isLive: true,
            dimmed: false,
            isOpen: isOpen("session:\(session.sessionId)"),
            isBusy: false,
            switchLabel: "",
            isOn: nil,
            setOn: { _ in }
        ) {
            AppGlyph(target: sessionTarget(session), name: name, appPath: session.host?.app)
        } detail: {
            ClientDetail(
                name: name,
                about: "Plug does not know this client by name, so it has no switch. It shows here while it is connected.",
                access: key.map { access(key: $0, name: name) },
                connections: [connection(session)],
                rename: names.key(of: session).map { key in { rename(key: key, shown: name) } },
                run: run
            )
        }
        .listRowSeparator(.hidden)
    }

    /// A client allowed in over the network. Its switch is its permission:
    /// off takes the permission away, and the client has to ask again.
    private func grantRow(_ grant: DownstreamClient) -> some View {
        let name = names.name(forKey: grant.clientKey) ?? grant.clientName
        let access = access(key: grant.clientKey, name: name)
        return GrantRow(
            grant: grant,
            name: name,
            access: access,
            isOpen: isOpen("grant:\(grant.clientId)"),
            rename: { rename(key: grant.clientKey, shown: name) },
            run: run
        )
        .listRowSeparator(.hidden)
    }

    private func connection(_ session: LiveSession) -> ClientDetail.Connection {
        ClientDetail.Connection(
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

/// One client: its icon, its name, one line about it, and one switch. The row
/// opens its detail; the switch works without opening anything.
private struct ClientRow<Glyph: View, Detail: View>: View {
    let name: String
    let status: ClientStatus
    let isLive: Bool
    let dimmed: Bool
    @Binding var isOpen: Bool
    let isBusy: Bool
    let switchLabel: String
    /// Nil when this client has nothing to switch.
    let isOn: Bool?
    let setOn: (Bool) -> Void
    @ViewBuilder var glyph: Glyph
    @ViewBuilder var detail: Detail

    var body: some View {
        HStack(spacing: Metric.snug) {
            Button { isOpen = true } label: {
                HStack(spacing: Metric.snug) {
                    glyph.opacity(dimmed ? 0.4 : 1)
                    VStack(alignment: .leading, spacing: Metric.rowGap) {
                        Text(name)
                            .font(.body)
                            .foregroundStyle(dimmed ? .secondary : .primary)
                        Label(status.text, systemImage: status.symbol)
                            .font(.caption)
                            .foregroundStyle(isLive ? Color.green : Color.secondary)
                            .labelStyle(.titleAndIcon)
                            .lineLimit(1)
                    }
                    Spacer(minLength: Metric.tight)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Show details")
            .accessibilityLabel("\(name), \(status.text)")
            .accessibilityHint("Shows details")
            .popover(isPresented: $isOpen, arrowEdge: .trailing) { detail }

            if isBusy {
                ProgressView().controlSize(.small)
            } else if let isOn {
                Toggle(switchLabel, isOn: Binding(get: { isOn }, set: setOn))
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .accessibilityLabel("\(switchLabel), \(name)")
            }
        }
        .padding(.vertical, Metric.tight)
    }
}

/// Everything about one client that is not its switch: the servers it can
/// use, what it has open, and its name.
private struct ClientDetail: View {
    /// One open connection of a client.
    struct Connection: Identifiable {
        let id: String
        let how: String
        let tools: String
    }

    let name: String
    let about: String
    /// Nil when there are no server choices to offer for this client.
    let access: ClientAccess?
    let connections: [Connection]
    /// Nil when Plug has nothing to store a name under.
    let rename: (() -> Void)?
    /// A line that identifies the client to someone checking it, such as the
    /// site it came from.
    var identity: String?
    let run: (PlugIntent) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(name).font(.headline)
                Text(about)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let identity {
                    Text(identity)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            }

            if let access {
                VStack(alignment: .leading, spacing: Metric.tight) {
                    SectionLabel(text: "Servers it can use")
                    if access.servers.isEmpty {
                        Text("No servers yet.").font(.callout).foregroundStyle(.secondary)
                    } else {
                        ScrollView {
                            VStack(spacing: Metric.tight) {
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
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                    }
                                    .toggleStyle(.switch)
                                    .controlSize(.small)
                                }
                            }
                        }
                        .frame(maxHeight: 220)
                        .scrollBounceBehavior(.basedOnSize)
                    }
                    Text(Self.note(for: access))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if !connections.isEmpty {
                VStack(alignment: .leading, spacing: Metric.tight) {
                    SectionLabel(
                        text: "Connected now",
                        trailing: connections.count == 1 ? nil : "\(connections.count) connections"
                    )
                    ForEach(connections) { connection in
                        HStack(spacing: Metric.snug) {
                            Text(connection.how).font(.caption)
                            Spacer(minLength: Metric.tight)
                            Text(connection.tools)
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.secondary)
                        }
                        .help("Connection \(connection.id.prefix(8))")
                    }
                }
            }

            if let rename {
                Button("Rename…", action: rename)
            }
        }
        .padding(Metric.regular)
        .frame(width: 320, alignment: .leading)
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

/// A client allowed in over the network. On means it has permission; turning
/// it off asks first, because the client has to ask again to come back.
private struct GrantRow: View {
    let grant: DownstreamClient
    /// The name to show: the owner's, else the one the client registered.
    let name: String
    let access: ClientAccess
    @Binding var isOpen: Bool
    let rename: () -> Void
    let run: (PlugIntent) -> Void
    @State private var confirming = false

    var body: some View {
        ClientRow(
            name: name,
            status: ClientStatus(
                text: access.summary.map { "\(origin) · \($0)" } ?? origin,
                symbol: "network"
            ),
            isLive: false,
            dimmed: false,
            isOpen: $isOpen,
            isBusy: false,
            switchLabel: "Allowed",
            isOn: true,
            setOn: { if !$0 { confirming = true } }
        ) {
            Image(systemName: "key.horizontal")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: 22)
                .accessibilityHidden(true)
        } detail: {
            ClientDetail(
                name: name,
                about: "You allowed this client to use Plug over the network. Turn it off to take that back.",
                access: access,
                connections: [],
                rename: rename,
                identity: identity,
                run: run
            )
        }
        .confirmationDialog(
            "Turn off \(name)?",
            isPresented: $confirming,
            titleVisibility: .visible
        ) {
            Button("Turn Off", role: .destructive) { run(.revokeClient(id: grant.clientId)) }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("It stops being able to use Plug right away, and leaves this list. To come back it has to ask you again.")
        }
    }

    /// Where the client came from, when it says: a site is a name a person
    /// can check. Anything else is simply over the network.
    private var origin: String {
        URL(string: grant.clientId)?.host() ?? "Allowed"
    }

    /// A client Plug registered gets a short id, since two can share a name.
    /// It is here, not in the row, for the rare time two need telling apart.
    private var identity: String {
        if let host = URL(string: grant.clientId)?.host() { return "From \(host)" }
        let short = grant.clientId.hasPrefix("plug_")
            ? grant.clientId.dropFirst(5) : Substring(grant.clientId)
        return "ID \(short.prefix(8))"
    }
}
