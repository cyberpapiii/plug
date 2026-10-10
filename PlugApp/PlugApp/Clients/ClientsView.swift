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

    /// `named` is the client a session's owner named it as, when that is a
    /// client Plug knows. It outranks what the session reports: a sign-in
    /// named Grok Bot is not Cursor's, though it says Cursor.
    init(
        apps: [LinkableApp],
        sessions: [LiveSession],
        named: (LiveSession) -> String? = { _ in nil }
    ) {
        var claimed = Set<String>()
        var connected: [Connected] = []
        var idle: [LinkableApp] = []
        for app in apps {
            let own = sessions.filter { session in
                if let target = named(session) { return target == app.target }
                return Self.targets(of: session).contains(app.target)
            }
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
        return "Unknown client \(sessionId.prefix(4))"
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
    /// Where the owner says each client runs.
    private let places: [String: String]

    init(
        visibility: [ClientVisibility],
        names: [ClientName],
        grants: [DownstreamClient] = [],
        places: [ClientPlace] = []
    ) {
        self.places = Dictionary(places.map { ($0.key, $0.place) }, uniquingKeysWith: { first, _ in first })
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
            grants: snapshot.downstreamClients,
            places: snapshot.clientPlaces ?? []
        )
    }

    /// Nil for a session the daemon cannot tell apart from other clients.
    /// There is nothing to store a name under, so it cannot be renamed.
    func key(of session: LiveSession) -> String? { keys[session.sessionId] }

    func name(forKey key: String?) -> String? { key.flatMap { names[$0] } }

    /// Where the owner says the client runs.
    func place(forKey key: String?) -> String? { key.flatMap { places[$0] } }

    /// Every place the owner has typed, in order.
    var ownerPlaces: [String] {
        Set(places.values).sorted { $0.localizedStandardCompare($1) == .orderedAscending }
    }

    /// The owner's name first, then a product Plug knows. A remote session
    /// Plug does not know takes the name on its grant, which the owner
    /// approved, ahead of whatever the session says about itself.
    func displayName(_ session: LiveSession) -> String {
        name(forKey: key(of: session)) ?? originalName(session)
    }

    /// What a session is called before the owner names it.
    func originalName(_ session: LiveSession) -> String {
        let key = key(of: session)
        return session.knownName
            // A link says which client it was written for; that outranks
            // the program that happened to start the connector.
            ?? key.flatMap(AppIcons.displayName(forTarget:))
            ?? key.flatMap { grantNames[$0] }
            ?? session.displayName
    }

    /// The client a name belongs to, when the name is one Plug knows. An
    /// owner who names a client "Grok Bot" has said what it is, whatever the
    /// client reports about itself.
    func knownTarget(named name: String?) -> String? {
        guard let name else { return nil }
        let target = AppIcons.target(forClientType: name)
        return AppIcons.displayName(forTarget: target) == nil ? nil : target
    }

    /// The name a remote client is pictured by: the owner's, when it is a
    /// client Plug knows, else the one on its grant.
    func picturedName(forGrant grant: DownstreamClient) -> String {
        let owned = name(forKey: grant.clientKey)
        return knownTarget(named: owned) == nil ? grant.clientName : (owned ?? grant.clientName)
    }

    /// The target whose icon a session shows: the client its owner named it
    /// as, else its own when the owner made it up in Add a Client, else the
    /// client it reports, else the client its link was written for, else
    /// the client its sign-in names. A call is pictured the same way.
    func target(of session: LiveSession) -> String {
        if let named = knownTarget(named: name(forKey: key(of: session))) { return named }
        if let key = key(of: session), key.hasPrefix(AddClient.customPrefix) { return key }
        let reported = AppIcons.target(forClientType: session.clientType)
        if AppIcons.displayName(forTarget: reported) != nil { return reported }
        if let key = key(of: session), AppIcons.displayName(forTarget: key) != nil {
            return key
        }
        if let grant = key(of: session).flatMap({ grantNames[$0] }) {
            return AppIcons.target(forClientType: grant)
        }
        return reported
    }

    /// The distinct clients behind a set of open sessions, first seen first,
    /// each as the Clients list names and pictures it.
    func connectedClients(_ sessions: [LiveSession]) -> [ConnectedClient] {
        var seen = Set<String>()
        return sessions.compactMap { session in
            let target = target(of: session)
            let known = AppIcons.displayName(forTarget: target) != nil
            // A client Plug knows is one client however it is keyed. Any
            // other is told apart by its key.
            let identity = known ? target : (key(of: session) ?? target)
            guard seen.insert(identity).inserted else { return nil }
            return ConnectedClient(
                target: target,
                name: displayName(session),
                appPath: session.host?.app
            )
        }
    }
}

/// Clients known only by the calls they made: not connected now, and with
/// no entry of their own.
enum RecentCallers {
    /// The last call of each client that is not already shown, newest
    /// first. A call with no key belongs to no client Plug can tell apart.
    static func latest(_ events: [ActivityEvent], excluding shown: Set<String>) -> [ActivityEvent] {
        var last: [String: ActivityEvent] = [:]
        for event in events {
            guard let key = event.clientKey, !key.isEmpty, !shown.contains(key) else { continue }
            if last[key].map({ $0.occurredAtMs < event.occurredAtMs }) ?? true { last[key] = event }
        }
        return last.values.sorted { $0.occurredAtMs > $1.occurredAtMs }
    }

    /// Whether a key belongs to a client that reaches Plug over the network.
    static func isRemote(key: String) -> Bool {
        key.hasPrefix("oauth:") || key.hasPrefix("remote:")
    }

    /// "Last used 11:15 AM" today, and with the day before that.
    static func lastUsed(_ date: Date, now: Date = Date(), calendar: Calendar = .current) -> String {
        let time = date.formatted(date: .omitted, time: .shortened)
        if calendar.isDate(date, inSameDayAs: now) { return "Last used \(time)" }
        return "Last used \(date.formatted(.dateTime.month(.abbreviated).day())), \(time)"
    }
}

/// The sections of the Clients column. A client sits under how it reaches
/// Plug unless the owner has said where it runs; then it sits under that.
enum ClientPlaces {
    /// The section a client is listed under.
    static func place(owners: String?, group: ClientEntry.Group) -> String {
        if group != .notUsing, let owners, !owners.isEmpty { return owners }
        return group.rawValue
    }

    /// The order the sections show in: on this Mac, the owner's places by
    /// name, over the network, then clients that do not use Plug.
    static func ordered(_ places: [String]) -> [String] {
        let groups = ClientEntry.Group.self
        let last = [groups.network.rawValue, groups.notUsing.rawValue, groups.unfinished.rawValue]
        let present = Set(places)
        let owners = present.subtracting(last + [groups.onThisMac.rawValue])
            .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
        return ([groups.onThisMac.rawValue] + owners + last).filter(present.contains)
    }
}

/// Telling one sign-in over the network from another by when it happened.
/// Pure values so the wording and the matching can be tested.
enum SignInFacts {
    /// How long a new sign-in keeps asking which client it is.
    static let newFor: TimeInterval = 14 * 86_400

    static func day(_ seconds: UInt64, now: Date = Date()) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(seconds))
        if Calendar.current.isDate(date, inSameDayAs: now) { return "today" }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }

    /// What an unfinished sign-in's row says in place of a switch.
    static func goneIn(expiresAt: UInt64?, now: Date = Date()) -> String {
        guard let expiresAt else { return "" }
        let left = TimeInterval(expiresAt) - now.timeIntervalSince1970
        if left < 60 { return "gone soon" }
        if left < 3_600 { return "gone in \(Int(left / 60)) min" }
        return "gone in \(Int(left / 3_600)) hr"
    }

    /// The earlier sign-ins a new one may be replacing: the ones that gave
    /// the same name. Empty once the owner has named the new one or answered.
    static func earlier(
        than grant: DownstreamClient,
        among grants: [DownstreamClient],
        ownerNamed: Bool,
        answered: Set<String>,
        now: Date = Date()
    ) -> [DownstreamClient] {
        guard let created = grant.createdAt, !grant.needsSignIn, !ownerNamed,
              !answered.contains(grant.clientId),
              now.timeIntervalSince1970 - TimeInterval(created) < newFor
        else { return [] }
        return grants.filter {
            $0.clientId != grant.clientId && !$0.isUnfinished
                && $0.clientName.caseInsensitiveCompare(grant.clientName) == .orderedSame
                && ($0.createdAt ?? 0) <= created
        }
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
    /// Tools kept from the client, lowercased, as the daemon compares
    /// them. A name with `*` in it is a rule and covers every tool it fits.
    let blockedTools: [String]
    var blockedToolCount: Int { blockedTools.count }
    /// The client gets only the servers and tools on its list, so a server
    /// added later is off for it until it is turned on.
    let onlyAllowed: Bool
    let allowedServers: Set<String>
    /// Servers of which only single tools are on the list.
    let partlyAllowedServers: Set<String>
    /// Those tools, lowercased.
    let allowedTools: Set<String>

    init(key: String, name: String, servers: [ConfiguredServer], blocks: [ClientBlocks]) {
        self.key = key
        self.name = name
        isRemote = key.hasPrefix("oauth:")
        self.servers = servers
        let blocks = blocks.first { $0.key == key }
        // A block on a server that is gone does nothing and is not counted.
        blockedServers = Set(blocks?.servers ?? []).intersection(servers.map(\.name))
        blockedTools = (blocks?.tools ?? []).map { $0.lowercased() }
        onlyAllowed = blocks?.onlyAllowed ?? false
        allowedServers = Set(blocks?.allowedServers ?? [])
        partlyAllowedServers = Set(blocks?.partlyAllowedServers ?? []).subtracting(allowedServers)
        allowedTools = Set((blocks?.allowedTools ?? []).map { $0.lowercased() })
    }

    /// Whether the client gets anything of `server`.
    func isOn(server: String) -> Bool {
        if blockedServers.contains(server) { return false }
        return !onlyAllowed || allowedServers.contains(server) || partlyAllowedServers.contains(server)
    }

    /// The servers the client gets nothing of.
    var offServers: [String] { servers.map(\.name).filter { !isOn(server: $0) } }

    /// Whether this tool's switch puts it on the list or takes it off: its
    /// server is not on the list whole. Otherwise the switch is a block.
    func isListedSingly(_ tool: ToolFacts) -> Bool {
        onlyAllowed && !allowedServers.contains(tool.server)
    }

    func state(of tool: ToolFacts) -> ToolState {
        if isListedSingly(tool), !allowedTools.contains(tool.name.lowercased()) { return .off }
        return state(ofTool: tool.name)
    }

    /// How a tool stands for this client.
    enum ToolState: Equatable {
        case on
        /// Off by its own name; a switch turns it back on.
        case off
        /// Off under a rule that covers other tools as well.
        case offByRule(String)
    }

    func state(ofTool name: String) -> ToolState {
        let name = name.lowercased()
        if blockedTools.contains(name) { return .off }
        if let rule = blockedTools.first(where: { $0.contains("*") && Self.rule($0, fits: name) }) {
            return .offByRule(rule)
        }
        return .on
    }

    /// How many of these tools the client is kept from.
    func offCount(among tools: [ToolFacts]) -> Int {
        tools.filter { state(of: $0) != .on }.count
    }

    /// Whether `text` fits a rule in which `*` stands for any run of
    /// characters, the way the daemon reads one.
    static func rule(_ rule: String, fits text: String) -> Bool {
        let parts = rule.components(separatedBy: "*")
        guard parts.count > 1 else { return rule == text }
        var rest = Substring(text)
        for (index, part) in parts.enumerated() where !part.isEmpty {
            if index == 0 {
                guard rest.hasPrefix(part) else { return false }
                rest = rest.dropFirst(part.count)
            } else if index == parts.count - 1 {
                return rest.hasSuffix(part)
            } else {
                guard let found = rest.range(of: part) else { return false }
                rest = rest[found.upperBound...]
            }
        }
        return true
    }

    var isLimited: Bool { !offServers.isEmpty || blockedToolCount > 0 }

    /// What the status line says when something is off for the client.
    var summary: String? {
        guard isLimited else { return nil }
        var parts: [String] = []
        if !offServers.isEmpty {
            let count = offServers.count
            parts.append(count == 1 ? "1 server" : "\(count) servers")
        }
        if blockedToolCount > 0 {
            parts.append(blockedToolCount == 1 ? "1 tool" : "\(blockedToolCount) tools")
        }
        return parts.joined(separator: " and ") + " off"
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
    @Environment(\.splitPane) private var pane

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
    /// Every app is placed against all sessions, then search narrows what
    /// shows, so a session never leaves its app's row because the app was
    /// filtered out.
    private var roster: AppRoster {
        let names = self.names
        return AppRoster(apps: allApps, sessions: sessions) {
            names.knownTarget(named: names.name(forKey: names.key(of: $0)))
        }
    }
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

    /// The open sessions no app claimed, sorted into the clients they belong
    /// to, so one client is one row however many connections it has open.
    private struct Unclaimed {
        /// Sessions of a client allowed in over the network, by its key.
        var byGrant: [String: [LiveSession]] = [:]
        /// Every other client, with all of its sessions.
        var clients: [(id: String, sessions: [LiveSession])] = []
    }

    private var unclaimed: Unclaimed {
        let grantKeys = Set(model.snapshot.downstreamClients.map(\.clientKey))
        var result = Unclaimed()
        var index: [String: Int] = [:]
        for session in roster.other {
            if let key = names.key(of: session), grantKeys.contains(key) {
                result.byGrant[key, default: []].append(session)
                continue
            }
            // A session whose key is shared with other clients stays a row of
            // its own.
            let id = "session:\(accessKey(of: session) ?? session.sessionId)"
            if let at = index[id] {
                result.clients[at].sessions.append(session)
            } else {
                index[id] = result.clients.count
                result.clients.append((id: id, sessions: [session]))
            }
        }
        return result
    }

    /// The network client whose access is being removed, while the app asks
    /// first.
    @State private var revoking: Revoking?
    /// The new sign-ins the owner has said which client they are, one id per
    /// line.
    @AppStorage("answeredClientSignIns") private var answeredSignIns = ""

    private var answered: Set<String> { Set(answeredSignIns.split(separator: "\n").map(String.init)) }

    /// Keeps the ids of clients that still exist, so the list cannot grow
    /// without end.
    private func answer(_ id: String) {
        let live = Set(model.snapshot.downstreamClients.map(\.clientId))
        answeredSignIns = answered.intersection(live).union([id]).sorted().joined(separator: "\n")
    }

    private struct Revoking {
        let id: String
        let name: String
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
    /// Over the Network; a choice made here would not reach it.
    private func access(to app: LinkableApp, sessions: [LiveSession]) -> ClientAccess? {
        guard app.linked || !sessions.isEmpty else { return nil }
        let overNetwork = app.transport?.lowercased() == "http"
            || sessions.contains { accessKey(of: $0) != app.target }
        guard !overNetwork else { return nil }
        return access(key: app.target, name: names.name(forKey: app.target) ?? app.name)
    }

    private static func isRemote(_ session: LiveSession) -> Bool {
        ["http", "streamable_http", "sse"].contains(session.transport.lowercased())
    }

    /// The key a session's requests carry, when it is one a block can be
    /// stored under. A remote session with no grant shares its key with every
    /// other such session, so it is not offered server choices of its own.
    private func accessKey(of session: LiveSession) -> String? {
        guard let key = names.key(of: session) else { return nil }
        return Self.isRemote(session) && !key.hasPrefix("oauth:") ? nil : key
    }

    var body: some View {
        let entries = self.entries
        return Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading clients")
            } else if model.initialDataUnavailable {
                UnavailablePage(verdict: model.verdict, run: run)
            } else if model.isLoadingConnectableApps && entries.isEmpty {
                LoadingPage(message: "Loading clients")
            } else if entries.isEmpty {
                if let error = model.connectableAppsError {
                    PagePane {
                        ContentUnavailableView {
                            PlugCharacterLabel(title: "Clients Unavailable", mood: .worried)
                        } description: {
                            Text(error)
                        } actions: {
                            Button("Try Again") { Task { await model.loadConnectableApps() } }
                                .buttonStyle(.borderedProminent)
                        }
                    }
                } else if search.trimmingCharacters(in: .whitespaces).isEmpty {
                    EmptyPage(
                        title: "No Clients",
                        message: "A client is an app that uses your servers, such as Claude or Cursor. When Plug finds one on this Mac, it shows here with a switch.",
                        actionTitle: "Add Client…",
                        actionIntent: .addClient,
                        secondaryTitle: "How Plug Works",
                        secondaryIntent: .showGuide,
                        run: run
                    )
                } else {
                    NoSearchResults(text: search)
                }
            } else {
                ListDetail {
                    List(selection: $router.selectedClient) {
                        if let error = model.connectableAppsError {
                            ProblemNote(title: "Plug could not look for clients on this Mac", reason: error)
                                .selectionDisabled()
                        }
                        ForEach(ClientPlaces.ordered(entries.map(\.place)), id: \.self) { place in
                            group(place, in: entries)
                        }
                    }
                } detail: {
                    if let selected = entries.first(where: { $0.id == router.selectedClient }) {
                        ClientDetail(
                            entry: selected,
                            tools: model.toolCatalog,
                            places: names.ownerPlaces,
                            canMutate: model.canMutate,
                            run: run
                        )
                            .id(selected.id)
                    } else {
                        NoSelection(item: "Client", icon: .clients)
                    }
                }
            }
        }
        .navigationSubtitle(connectionSummary ?? "")
        .toolbar {
            // The window draws a section once per column; the button goes
            // above the list.
            if pane != .detail {
                ToolbarItem(placement: .primaryAction) {
                    Button { run(.addClient) } label: {
                        Label("Add Client", icon: .add)
                    }
                    .help("Add a client")
                }
            }
        }
        .onChange(of: entries.map(\.id), initial: true) { keepSelectionVisible() }
        .confirmationDialog(
            "Remove \(revoking?.name ?? "")'s access?",
            isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            titleVisibility: .visible,
            presenting: revoking
        ) { client in
            Button("Remove Access", role: .destructive) { run(.revokeClient(id: client.id)) }
            Button("Cancel", role: .cancel) { }
        } message: { _ in
            Text("It can no longer use Plug and leaves this list. To come back, it has to ask you again.")
        }
        // Links change outside the app too (`plug link`, another tool editing
        // a client's settings), so the list is read again while it is shown.
        .task {
            while !Task.isCancelled {
                await model.loadConnectableApps()
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }

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
    private func group(_ place: String, in entries: [Entry]) -> some View {
        let rows = entries.filter { $0.place == place }
        if !rows.isEmpty {
            ListGroupHeader(place)
            ForEach(rows) { entry in
                ClientRow(entry: entry, canMutate: model.canMutate).tag(entry.id)
            }
            .listRowSeparator(.hidden)
        }
    }

    // MARK: - Entries

    /// Every client the list shows, in the order it shows them. The list and
    /// the detail beside it both read from here, so they cannot disagree.
    private var entries: [Entry] {
        let unclaimed = self.unclaimed
        let others = unclaimed.clients
            .filter { client in
                client.sessions.contains {
                    matches(displayName($0)) || matches($0.transport) || matches($0.sessionId)
                }
            }
            .map { sessionEntry(id: $0.id, sessions: $0.sessions) }
        let represented = Set(allApps.filter { app in
            ClientStatus.hasNetworkRepresentation(app, grantNames: model.snapshot.downstreamClients.map(\.clientName))
        }.map(\.target))
        let apps = connectedApps.filter { !represented.contains($0.app.target) }.map { appEntry($0.app, sessions: $0.sessions) }
        let idle = idleApps.filter { !represented.contains($0.target) }.map { appEntry($0, sessions: []) }
        let granted = grants.map { grant in
            let target = AppIcons.target(forClientType: grant.clientName)
            var own = sessions.filter { names.key(of: $0) == grant.clientKey }
            // A uniquely identified grant can show its still-open local
            // sessions too. Multiple grants remain separate identities.
            if represented.contains(target), model.snapshot.downstreamClients.filter({
                AppIcons.target(forClientType: $0.clientName) == target
            }).count == 1 {
                own += sessions.filter { !Self.isRemote($0) && AppRoster.targets(of: $0).contains(target) }
            }
            return grantEntry(grant, sessions: own)
        }
        // A client that connects, calls, and is gone again has no row of
        // its own above. Its last call puts it here.
        let shown = Set((apps + idle + others + granted).flatMap { [$0.renameKey, $0.placeKey].compactMap { $0 } })
            .union(allApps.map(\.target))
            .union(sessions.compactMap(names.key(of:)))
        let recent = RecentCallers.latest(model.activities, excluding: shown)
            .map(recentEntry)
            .filter { matches($0.name) }
        let all = apps
            + others.filter { $0.group == .onThisMac }
            + recent.filter { $0.group == .onThisMac }
            + idle
            + others.filter { $0.group == .network }
            + granted
            + recent.filter { $0.group == .network }
        // Only clients that share a name need telling apart.
        let shared = Dictionary(grouping: all, by: \.name).filter { $0.value.count > 1 }
        return all.map { entry in
            guard shared[entry.name] == nil else { return entry }
            var entry = entry
            entry.disambiguator = nil
            return entry
        }
    }

    /// A client on this Mac. Its switch adds Plug to the client's settings or
    /// takes it out.
    private func appEntry(_ app: LinkableApp, sessions: [LiveSession]) -> Entry {
        let name = names.name(forKey: app.target) ?? app.name
        let access = access(to: app, sessions: sessions)
        let known = app.detected || app.linked
        let about = if !app.detected {
            "Plug cannot find this client on this Mac."
        } else if app.linked {
            "Plug is in this client's settings. Restart the client after changing this."
        } else {
            "No direct Plug entry in this client's local settings. A hosted connector may already provide access; see its network authorization above."
        }
        return Entry(
            id: "app:\(app.target)",
            group: !app.linked && sessions.isEmpty ? .notUsing : .onThisMac,
            place: ClientPlaces.place(
                owners: names.place(forKey: app.target),
                group: !app.linked && sessions.isEmpty ? .notUsing : .onThisMac
            ),
            name: name,
            originalName: app.name,
            status: ClientStatus.app(app, connections: sessions.count, limit: access?.summary),
            isLive: !sessions.isEmpty,
            dimmed: !known && sessions.isEmpty,
            glyph: .app(target: app.target, name: app.name, appPath: nil),
            isBusy: model.busyApps.contains(app.target),
            switchLabel: "Use Plug",
            isOn: known ? app.linked : nil,
            setOn: { run($0 ? .linkApp(app.target) : .unlinkApp(app.target)) },
            about: about,
            access: access,
            renameKey: access?.key,
            placeKey: app.target
        )
    }

    /// Something connected that Plug has no client entry for, with every
    /// session it has open. There is nothing to switch: it is here because it
    /// is connected.
    private func sessionEntry(id: String, sessions: [LiveSession]) -> Entry {
        let first = sessions[0]
        let name = displayName(first)
        let choices = accessKey(of: first).map { access(key: $0, name: name) }
        return Entry(
            id: id,
            group: Self.isRemote(first) ? .network : .onThisMac,
            place: ClientPlaces.place(
                owners: names.place(forKey: names.key(of: first)),
                group: Self.isRemote(first) ? .network : .onThisMac
            ),
            name: name,
            originalName: names.originalName(first),
            status: ClientStatus(
                state: ClientStatus.connected(sessions.count),
                limit: choices?.summary
            ),
            isLive: true,
            dimmed: false,
            glyph: .app(target: sessionTarget(first), name: name, appPath: first.host?.app),
            isBusy: false,
            switchLabel: "",
            isOn: nil,
            setOn: { _ in },
            about: "Plug does not recognize this client, so it has no switch. It shows here while it is connected.",
            access: choices,
            renameKey: names.key(of: first),
            placeKey: names.key(of: first),
            disambiguator: String(first.sessionId.prefix(4))
        )
    }

    /// A client that is not connected now and has no entry of its own, by
    /// the last call it made. A script that runs on a timer is one.
    private func recentEntry(_ event: ActivityEvent) -> Entry {
        let call = model.call(event)
        let key = event.clientKey ?? ""
        let group: Entry.Group = RecentCallers.isRemote(key: key) ? .network : .onThisMac
        // Every remote client without a sign-in of its own shares one key,
        // so there are no choices to offer for one of them.
        let choices = key.hasPrefix("remote:") ? nil : access(key: key, name: call.caller)
        return Entry(
            id: "recent:\(key)",
            group: group,
            place: ClientPlaces.place(owners: names.place(forKey: key), group: group),
            name: call.caller,
            originalName: CallFacts(event, grantName: call.grantName).caller,
            status: ClientStatus(
                state: RecentCallers.lastUsed(Date(timeIntervalSince1970: Double(event.occurredAtMs) / 1000)),
                limit: choices?.summary
            ),
            isLive: false,
            dimmed: false,
            glyph: .app(target: call.callerTarget, name: call.callerIconName, appPath: nil),
            isBusy: false,
            switchLabel: "",
            isOn: nil,
            setOn: { _ in },
            about: "Not connected now. This client connects when it has something to do and leaves when it is done, so Plug lists it by its last call. It stays here while that call is in Activity.",
            access: choices,
            renameKey: choices == nil ? nil : key,
            placeKey: choices == nil ? nil : key
        )
    }

    /// A client allowed in over the network, with the sessions it has open.
    /// Its switch is its permission: off takes the permission away, and the
    /// client has to ask again, so the app asks first.
    private func grantEntry(_ grant: DownstreamClient, sessions: [LiveSession]) -> Entry {
        let owned = names.name(forKey: grant.clientKey)
        let name = owned ?? grant.clientName
        if grant.isUnfinished { return unfinishedEntry(grant, name: name) }
        let access = access(key: grant.clientKey, name: name)
        let host = URL(string: grant.clientId)?.host()
        let since = grant.createdAt.map { SignInFacts.day($0) }
        // A client with a connection open is plainly signed in.
        let needsSignIn = grant.needsSignIn && sessions.isEmpty
        let target = AppIcons.target(forClientType: names.picturedName(forGrant: grant))
        let earlier = SignInFacts.earlier(
            than: grant, among: model.snapshot.downstreamClients, ownerNamed: owned != nil, answered: answered
        )
        return Entry(
            id: "grant:\(grant.clientId)",
            group: .network,
            place: ClientPlaces.place(owners: names.place(forKey: grant.clientKey), group: .network),
            name: name,
            originalName: grant.clientName,
            status: ClientStatus.grant(
                connections: sessions.count, needsSignIn: grant.needsSignIn, limit: access.summary
            ),
            isLive: !sessions.isEmpty,
            dimmed: false,
            glyph: .grant(name: names.picturedName(forGrant: grant)),
            isBusy: false,
            switchLabel: "Access",
            isOn: true,
            setOn: { allowed in
                if !allowed { revoking = Revoking(id: grant.clientId, name: name) }
            },
            about: needsSignIn
                ? ClientSignIn.about(target: target, name: name)
                : [since.map { "Signed in \($0)." }, "Turn this off to remove its access."]
                    .compactMap { $0 }.joined(separator: " "),
            access: access,
            renameKey: grant.clientKey,
            placeKey: grant.clientKey,
            disambiguator: host ?? since.map { $0 == "today" ? $0 : "since \($0)" },
            grantID: grant.clientId,
            needsSignIn: needsSignIn,
            signInCommand: needsSignIn ? ClientSignIn.command(target: target) : nil,
            asking: earlier.isEmpty ? nil : Entry.Asking(
                when: since ?? "",
                choices: earlier.map { other in
                    Entry.Asking.Choice(
                        id: other.clientId,
                        name: names.name(forKey: other.clientKey)
                            ?? [other.clientName, other.createdAt.map { "since \(SignInFacts.day($0))" }]
                                .compactMap { $0 }.joined(separator: ", ")
                    )
                },
                replace: { run(.replaceClient(oldID: $0, newID: grant.clientId)); answer(grant.clientId) },
                keep: { answer(grant.clientId) }
            )
        )
    }

    /// Something asked to be let in and nobody finished the sign-in. It has
    /// no access, so there is nothing to choose for it; its switch forgets
    /// it now.
    private func unfinishedEntry(_ grant: DownstreamClient, name: String) -> Entry {
        let until = grant.expiresAt.map {
            Date(timeIntervalSince1970: TimeInterval($0)).formatted(date: .omitted, time: .shortened)
        }
        return Entry(
            id: "grant:\(grant.clientId)",
            group: .unfinished,
            place: Entry.Group.unfinished.rawValue,
            name: name,
            originalName: grant.clientName,
            status: ClientStatus(state: "Sign-in never finished", limit: nil),
            isLive: false,
            dimmed: true,
            glyph: .grant(name: name),
            isBusy: false,
            switchLabel: "Access",
            isOn: nil,
            setOn: { _ in },
            about: "Something asked to use Plug under this name, and the sign-in was never finished. It cannot use anything."
                + (until.map { " Plug forgets it at \($0)." } ?? ""),
            access: nil,
            renameKey: nil,
            placeKey: nil,
            grantID: grant.clientId,
            note: SignInFacts.goneIn(expiresAt: grant.expiresAt),
            forget: { run(.revokeClient(id: grant.clientId)) }
        )
    }

    private func matches(_ value: String) -> Bool {
        let query = search.trimmingCharacters(in: .whitespaces)
        return query.isEmpty || value.localizedCaseInsensitiveContains(query)
    }

    private var connectionSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let unclaimed = self.unclaimed
        let count = roster.connected.count + unclaimed.clients.count + unclaimed.byGrant.count
        let summary = count == 0 ? "None connected" : "\(count) connected"
        return model.dataIsStale ? "Last known · \(summary)" : summary
    }

    private func displayName(_ session: LiveSession) -> String { names.displayName(session) }

    /// The target whose icon a session shows: the client it reports, else the
    /// client its link was written for.
    private func sessionTarget(_ session: LiveSession) -> String { names.target(of: session) }
}

/// The one line under a client's name in its detail. Pure value so the
/// wording can be tested.
struct ClientStatus: Equatable {
    let text: String

    static func hasNetworkRepresentation(_ app: LinkableApp, grantNames: [String]) -> Bool {
        app.linked && app.transport?.lowercased() == "http"
            && grantNames.contains { AppIcons.target(forClientType: $0) == app.target }
    }

    /// A state, followed by what is off for the client when something is.
    init(state: String, limit: String?) {
        text = limit.map { "\(state) · \($0)" } ?? state
    }

    /// The state of a client with this many open connections.
    static func connected(_ connections: Int) -> String {
        connections == 1 ? "Connected" : "\(connections) connections"
    }

    /// A client allowed in over the network. One with no sign-in left says
    /// so, because nothing else about it looks different.
    static func grant(connections: Int, needsSignIn: Bool, limit: String?) -> ClientStatus {
        let state = if connections > 0 {
            connected(connections)
        } else if needsSignIn {
            "Needs sign-in"
        } else {
            "Allowed over the network"
        }
        return ClientStatus(state: state, limit: limit)
    }

    /// A client on this Mac: connected, ready, or not using Plug.
    static func app(_ app: LinkableApp, connections: Int, limit: String?) -> ClientStatus {
        let state: String
        if connections > 0 {
            state = connected(connections)
        } else if !app.detected {
            state = "Not found on this Mac"
        } else if !app.linked {
            state = "No local Plug configuration"
        } else {
            state = app.transport?.lowercased() == "http" ? "Uses Plug over the network" : "Not open"
        }
        return ClientStatus(state: state, limit: limit)
    }
}

/// One client as the list and the detail show it.
struct ClientEntry: Identifiable {
    /// Where a client is. The title is also what a connection row says.
    enum Group: String {
        case onThisMac = "On This Mac"
        case network = "Over the Network"
        case notUsing = "No Local Plug Configuration"
        case unfinished = "Unfinished Sign-Ins"
    }

    enum Glyph {
        case app(target: String, name: String, appPath: String?)
        /// A client allowed in over the network, by the name it gave.
        case grant(name: String)

        /// The name a chosen icon for this client is kept under.
        var iconKey: String {
            switch self {
            case let .app(target, _, _): IconStore.key(client: target)
            case let .grant(name): IconStore.key(client: AppIcons.target(forClientType: name))
            }
        }
    }

    /// A new sign-in that gave a name earlier ones gave, and the question
    /// that settles which client it is.
    struct Asking {
        struct Choice: Identifiable {
            /// The earlier sign-in's client id.
            let id: String
            let name: String
        }

        /// The day it signed in, as the row says it.
        let when: String
        let choices: [Choice]
        /// It is this earlier client, signed in again.
        let replace: (String) -> Void
        /// It is a client of its own.
        let keep: () -> Void
    }

    let id: String
    let group: Group
    /// Where it runs, which is the section it shows under.
    let place: String
    let name: String
    /// What the client is called when the owner has not named it.
    let originalName: String
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
    /// Nil when Plug has nothing to store a name under.
    let renameKey: String?
    /// The key its place is stored under. An app set up on this Mac has one
    /// even when it reaches Plug over the network and cannot be renamed.
    let placeKey: String?
    /// The owner has said where this client runs.
    var hasOwnPlace: Bool { place != group.rawValue }
    /// A few words that tell this client from another with the same name.
    /// Nil when its name is the only one like it.
    var disambiguator: String?
    /// Set for a client allowed in over the network.
    var grantID: String?
    /// Its sign-in ended and it has to sign in again.
    var needsSignIn = false
    /// What Plug runs to sign it in again, when the client has a command.
    var signInCommand: String?
    /// Set while a new sign-in has not been told apart from earlier ones.
    var asking: Asking?
    /// A few words a row says in place of a switch.
    var note: String?
    /// Forgets an unfinished sign-in now.
    var forget: (() -> Void)?
}

private typealias Entry = ClientEntry

private struct ClientGlyph: View {
    let glyph: ClientEntry.Glyph
    /// The large one fills a detail header's glyph slot.
    var large = false

    var body: some View {
        let size = large ? Metric.glyphSlot : 18
        switch glyph {
        case let .app(target, name, appPath):
            AppGlyph(target: target, name: name, appPath: appPath, size: size)
        case let .grant(name):
            let target = AppIcons.target(forClientType: name)
            AppGlyph(target: target, name: target == "python" ? "Python" : name, size: size)
        }
    }
}

/// One client: its icon, its name, and one switch. The switch works without
/// selecting the row.
private struct ClientRow: View {
    let entry: ClientEntry
    let canMutate: Bool

    private static let liveDot: CGFloat = 7

    var body: some View {
        HStack(spacing: Metric.tight) {
            ClientGlyph(glyph: entry.glyph)
                .opacity(entry.dimmed ? 0.4 : 1)
            Text(entry.name)
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(entry.dimmed ? .secondary : .primary)
                .layoutPriority(1)
                .accessibilityLabel("\(entry.name), \(entry.status.text)")
            if let disambiguator = entry.disambiguator {
                Text(disambiguator)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: Metric.tight)
            if entry.asking != nil {
                Text("New")
                    .font(PanelType.small.weight(.medium))
                    .padding(.horizontal, Metric.tight)
                    .background(.tint.opacity(0.2), in: Capsule())
                    .accessibilityLabel("New sign-in")
            }
            if let note = entry.note {
                Text(note)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            if entry.needsSignIn {
                PlugIcon(.needsYou)
                    .foregroundStyle(StatusColor.needsYou)
                    .help("Needs sign-in")
                    .accessibilityLabel("Needs sign-in")
            }
            if entry.isLive {
                Circle()
                    .fill(StatusColor.working)
                    .frame(width: Self.liveDot, height: Self.liveDot)
                    .accessibilityLabel("Connected")
            }
            ClientSwitch(entry: entry, canMutate: canMutate, size: .mini)
        }
        .contextMenu {
            if entry.grantID == nil, let isOn = entry.isOn, !entry.isBusy {
                Button(isOn ? "Turn Off" : "Turn On") { entry.setOn(!isOn) }
                Divider()
            }
            IconMenu(key: entry.glyph.iconKey)
            if let forget = entry.forget, canMutate {
                Divider()
                Button("Forget Now", role: .destructive, action: forget)
            } else if entry.grantID != nil, canMutate {
                Divider()
                Button("Remove Access…", role: .destructive) { entry.setOn(false) }
            }
        }
    }
}

/// A client's one switch. A switch that adds Plug to another app's settings
/// works while Plug is off; one that takes away access needs Plug running.
private struct ClientSwitch: View {
    let entry: ClientEntry
    let canMutate: Bool
    var size: ControlSize = .regular

    private var isGrant: Bool { entry.grantID != nil }

    var body: some View {
        if let isOn = entry.isOn {
            Toggle(entry.switchLabel, isOn: Binding(get: { isOn }, set: entry.setOn))
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(size)
                .disabled(entry.isBusy || (isGrant && !canMutate))
                .help(isGrant ? "Remove Access…" : "")
                .accessibilityLabel("\(entry.switchLabel), \(entry.name)")
        }
    }
}

/// Everything about one client. Its icon, its name, its switch and its two
/// settings are the first thing on the page; the servers it can use follow.
private struct ClientDetail: View {
    let entry: ClientEntry
    let tools: ToolCatalog
    /// The places the Runs On menu offers.
    let places: [String]
    let canMutate: Bool
    let run: (PlugIntent) -> Void
    /// The name as it is being typed.
    @State private var draft: String
    @FocusState private var naming: Bool
    /// A place being typed, while the sheet that asks for it is up.
    @State private var newPlace: String?
    /// The servers whose tools are showing.
    @State private var opened: Set<String> = []

    init(
        entry: ClientEntry,
        tools: ToolCatalog,
        places: [String],
        canMutate: Bool,
        run: @escaping (PlugIntent) -> Void
    ) {
        self.entry = entry
        self.tools = tools
        self.places = places
        self.canMutate = canMutate
        self.run = run
        _draft = State(initialValue: entry.name)
    }

    var body: some View {
        DetailForm {
            if let asking = entry.asking {
                Section {
                    question(asking)
                }
            }

            Section {
                header
                if let command = entry.signInCommand {
                    ProblemNote(
                        title: "\(entry.name) needs to sign in again.",
                        actionTitle: "Sign In",
                        action: { run(.signInClient(name: entry.name, command: command)) }
                    )
                    .disabled(!canMutate)
                }
                if let key = entry.placeKey, entry.group != .notUsing {
                    runsOn(key: key)
                }
                if let access = entry.access, !access.servers.isEmpty {
                    Toggle(
                        "New Servers",
                        isOn: Binding(
                            get: { !access.onlyAllowed },
                            set: { run(.setClientAllowList(key: access.key, on: !$0)) }
                        )
                    )
                    .disabled(!canMutate)
                    .help("When this is off, a server you add later stays off for this client until you turn it on below.")
                }
                if let forget = entry.forget {
                    Button("Forget Now", role: .destructive, action: forget)
                        .disabled(!canMutate)
                }
            } footer: {
                Text(entry.about)
            }

            if let access = entry.access {
                Section {
                    if access.servers.isEmpty {
                        Text("No servers yet").foregroundStyle(.secondary)
                    } else {
                        ForEach(access.servers) { server in
                            serverRow(server, access: access)
                        }
                    }
                } header: {
                    Text("Servers")
                } footer: {
                    Text(Self.note(for: access))
                }
            }
        }
    }

    /// The icon is a menu and the name is a field, so both change where they
    /// are shown.
    private var header: some View {
        HStack(spacing: Metric.snug) {
            Menu {
                IconMenu(key: entry.glyph.iconKey)
            } label: {
                ClientGlyph(glyph: entry.glyph, large: true)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .frame(width: Metric.glyphSlot, height: Metric.glyphSlot)
            .help("Change the icon")
            .accessibilityLabel("Icon of \(entry.name)")
            VStack(alignment: .leading, spacing: Metric.hairline) {
                if let key = entry.renameKey {
                    HStack(spacing: Metric.tight) {
                        TextField("Name", text: $draft, prompt: Text(entry.originalName))
                            .textFieldStyle(.plain)
                            .labelsHidden()
                            .font(.title3.weight(.semibold))
                            .focused($naming)
                            .onSubmit { rename(key) }
                            .onChange(of: naming) { if !naming { rename(key) } }
                            .disabled(!canMutate)
                            .help("Click to rename. Only Plug shows this name.")
                        PlugIcon(.edit, size: 14)
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                    }
                } else {
                    Text(entry.name)
                        .font(.title3.weight(.semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Text(entry.status.text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            ClientSwitch(entry: entry, canMutate: canMutate)
        }
    }

    /// An empty name, or the client's own, goes back to the client's own.
    private func rename(_ key: String) {
        let typed = draft.trimmingCharacters(in: .whitespaces)
        let name = typed == entry.originalName ? "" : typed
        if typed.isEmpty { draft = entry.originalName }
        guard (name.isEmpty ? entry.originalName : name) != entry.name else { return }
        run(.renameClient(key: key, name: name))
    }

    /// One question, asked once: a new sign-in gave a name that earlier ones
    /// gave, and only the owner knows whether it is one of them.
    private func question(_ asking: ClientEntry.Asking) -> some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            Text("New sign-in \(asking.when). Which client is this?")
                .font(.callout.weight(.medium))
            Text("It calls itself \(entry.originalName), like others here. Pick one and this sign-in takes over its name and choices, and the old sign-in is removed.")
                .font(PanelType.small)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                ForEach(asking.choices) { choice in
                    Button(choice.name) { asking.replace(choice.id) }
                }
                Button("A New Client", action: asking.keep)
            }
            .disabled(!canMutate)
        }
    }

    /// Where the client runs: the places already in use, or a new one.
    private func runsOn(key: String) -> some View {
        LabeledContent("Runs On") {
            Menu(entry.hasOwnPlace ? entry.place : "Not Set") {
                ForEach(places, id: \.self) { place in
                    Button(place) { run(.setClientPlace(key: key, place: place)) }
                }
                if !places.isEmpty { Divider() }
                Button("New Place…") { newPlace = "" }
                if entry.hasOwnPlace {
                    Button("Not Set") { run(.setClientPlace(key: key, place: "")) }
                }
            }
            .fixedSize()
            .disabled(!canMutate)
        }
        .alert("Where does \(entry.name) run?", isPresented: Binding(
            get: { newPlace != nil },
            set: { if !$0 { newPlace = nil } }
        )) {
            TextField("Work laptop", text: Binding(get: { newPlace ?? "" }, set: { newPlace = $0 }))
            Button("Save") {
                let place = (newPlace ?? "").trimmingCharacters(in: .whitespaces)
                if !place.isEmpty { run(.setClientPlace(key: key, place: place)) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("A name for the computer this client runs on. Clients with a place are listed under it.")
        }
    }

    /// One server's switch for this client, and under it, when opened, a
    /// switch for each of its tools.
    @ViewBuilder private func serverRow(_ server: ConfiguredServer, access: ClientAccess) -> some View {
        let serverOn = access.isOn(server: server.name)
        let own = tools.tools(for: server.name).filter(\.isOn)
        let off = access.offCount(among: own)
        let isOpen = opened.contains(server.name) && server.enabled && serverOn && !own.isEmpty
        HStack(spacing: Metric.tight) {
            Button {
                if isOpen { opened.remove(server.name) } else { opened.insert(server.name) }
            } label: {
                Image(systemName: "chevron.right")
                    .font(PanelType.small.weight(.semibold))
                    .rotationEffect(.degrees(isOpen ? 90 : 0))
                    .frame(width: 12)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .opacity(server.enabled && serverOn && !own.isEmpty ? 1 : 0)
            .accessibilityLabel(isOpen ? "Hide \(server.name) Tools" : "Show \(server.name) Tools")
            .help(isOpen ? "Hide Tools" : "Show Tools")
            ServerGlyph(name: server.name).opacity(server.enabled ? 1 : 0.4)
            Text(server.name)
            Spacer(minLength: Metric.tight)
            if !server.enabled {
                // Off in Plug, so no client can use it.
                Text("Off")
            } else if serverOn, off > 0 {
                Text(off == 1 ? "1 tool off" : "\(off) tools off")
                    .font(.callout)
            }
            Toggle(
                server.name,
                isOn: Binding(
                    get: { serverOn },
                    set: {
                        run(
                            access.onlyAllowed
                                ? .setClientServerAllowed(key: access.key, server: server.name, allowed: $0)
                                : .setClientServerBlocked(key: access.key, server: server.name, blocked: !$0)
                        )
                    }
                )
            )
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.mini)
            .disabled(!canMutate || !server.enabled)
        }
        .foregroundStyle(server.enabled ? .primary : .secondary)
        if isOpen {
            ForEach(own) { tool in
                toolRow(tool, access: access)
            }
        }
    }

    private func toolRow(_ tool: ToolFacts, access: ClientAccess) -> some View {
        let state = access.state(of: tool)
        return HStack(spacing: Metric.tight) {
            ToolName(tool.shortName, dimmed: state != .on)
                .help(tool.summary ?? tool.shortName)
            Spacer(minLength: Metric.tight)
            if case let .offByRule(rule) = state {
                Label("Off by Rule", icon: .locked)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .help("A rule in the settings file (\(rule)) keeps this tool from this client. Remove the rule to turn it back on.")
            } else {
                Toggle(
                    tool.shortName,
                    isOn: Binding(
                        get: { state == .on },
                        set: {
                            run(
                                access.isListedSingly(tool)
                                    ? .setClientToolAllowed(key: access.key, tool: tool.name, allowed: $0)
                                    : .setClientToolBlocked(key: access.key, tool: tool.name, blocked: !$0)
                            )
                        }
                    )
                )
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
                .disabled(!canMutate)
            }
        }
        .padding(.leading, 44)
    }

    static func note(for access: ClientAccess) -> String {
        access.isRemote
            ? "Turn a server off to keep this client from using its tools. Open a server to turn off single tools."
            : "Turn a server off to hide its tools from this client, or open it to hide single tools. This tidies the list; it is not a security lock."
    }
}
