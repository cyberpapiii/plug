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

/// Who can use Plug. The old app split this in two — "Clients" listed apps and
/// "Auth" listed the grants for the same apps — so the audit question ("who
/// reaches my tools, and how do I cut them off?") could not be answered in one
/// place. It can now.
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
    @State private var expanded: Set<String> = []
    @State private var renaming: Renaming?
    @State private var newName = ""

    /// The client being renamed.
    private struct Renaming {
        let key: String
        /// Set when the owner already named it, so the name can be taken back.
        let hasName: Bool
    }

    private func rename(key: String, shown: String) {
        newName = shown
        renaming = Renaming(key: key, hasName: names.name(forKey: key) != nil)
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
                    EmptyPage(
                        title: search.isEmpty
                            ? (model.connectableAppsError == nil ? "Nothing is connected" : "Client scan failed")
                            : "No matching clients",
                        message: search.isEmpty
                            ? (model.connectableAppsError
                                ?? "When a client connects through Plug it shows up here, along with everything it can reach.")
                            : "Nothing connected to Plug matches “\(search.trimmingCharacters(in: .whitespaces))”.",
                        symbol: search.isEmpty
                            ? (model.connectableAppsError == nil
                                ? "app.connected.to.app.below.fill"
                                : "exclamationmark.triangle")
                            : "magnifyingglass"
                    )
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
                                count: connectedApps.count + unmatchedSessions.count,
                                unit: "client"
                            )
                            ForEach(connectedApps) { entry in
                                AppLinkRow(
                                    app: entry.app,
                                    name: names.name(forKey: entry.app.target) ?? entry.app.name,
                                    sessionCount: entry.sessions.count,
                                    isExpanded: expansion(entry.app.target),
                                    isBusy: model.busyApps.contains(entry.app.target),
                                    run: run
                                )
                                .listRowSeparator(.hidden)
                                if expanded.contains(entry.app.target) {
                                    ForEach(entry.sessions) { session in
                                        sessionLine(session)
                                            .listRowSeparator(.hidden)
                                    }
                                }
                            }
                            ForEach(unmatchedSessions) { session in
                                sessionRow(session)
                                    .listRowSeparator(.hidden)
                            }
                        }
                        if !idleApps.isEmpty {
                            sectionLabel("On this Mac", count: idleApps.count, unit: "client")
                            ForEach(idleApps) { app in
                                AppLinkRow(
                                    app: app,
                                    name: names.name(forKey: app.target) ?? app.name,
                                    sessionCount: 0,
                                    isExpanded: nil,
                                    isBusy: model.busyApps.contains(app.target),
                                    run: run
                                )
                                .listRowSeparator(.hidden)
                            }
                            Text("Turn on a client to add Plug to its settings. Restart it to pick up the change.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                        }
                        if !grants.isEmpty {
                            sectionLabel("Remote", count: grants.count, unit: "client")
                            ForEach(grants) { grant in
                                GrantRow(
                                    grant: grant,
                                    name: names.name(forKey: grant.clientKey) ?? grant.clientName,
                                    rename: { rename(key: grant.clientKey, shown: $0) },
                                    run: run
                                )
                                    .listRowSeparator(.hidden)
                            }
                            Text("These clients can reach Plug over the network. Revoke anything you don't recognize.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
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

    private func sectionLabel(_ title: String, count: Int, unit: String) -> some View {
        SectionLabel(text: title, trailing: count == 1 ? "1 \(unit)" : "\(count) \(unit)s")
            .padding(.top, Metric.regular)
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
    }

    private func expansion(_ target: String) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(target) },
            set: { isOn in
                if isOn { expanded.insert(target) } else { expanded.remove(target) }
            }
        )
    }

    /// One session of an app whose row is already above it: the id that tells
    /// it apart, how long it has been open, and what it can reach.
    private func sessionLine(_ session: LiveSession) -> some View {
        HStack(spacing: Metric.snug) {
            Text(session.sessionId.prefix(8))
                .font(.caption.monospaced())
                .textSelection(.enabled)
            Text(duration(session.connectedSecs))
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer(minLength: Metric.tight)
            Text(toolsText(session))
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding(.leading, 22 + Metric.snug)
        .accessibilityElement(children: .combine)
    }

    private func matches(_ value: String) -> Bool {
        let query = search.trimmingCharacters(in: .whitespaces)
        return query.isEmpty || value.localizedCaseInsensitiveContains(query)
    }

    private var connectionSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = sessions.count
        let summary = "\(count) open \(count == 1 ? "session" : "sessions")"
        return model.dataIsStale ? "Last known · \(summary)" : summary
    }

    private func sessionRow(_ session: LiveSession) -> some View {
        HStack(spacing: Metric.snug) {
            AppGlyph(
                target: sessionTarget(session),
                name: displayName(session),
                appPath: session.host?.app
            )
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(displayName(session)).font(.body)
                Text(connectionDescription(session))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            Text(toolsText(session))
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
            if let key = names.key(of: session) {
                RenameButton { rename(key: key, shown: displayName(session)) }
            }
        }
        .padding(.vertical, Metric.tight)
        .accessibilityElement(children: .combine)
        .accessibilityActions {
            if let key = names.key(of: session) {
                Button("Rename") { rename(key: key, shown: displayName(session)) }
            }
        }
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
        else { return "—" }
        return count == 1 ? "1 tool" : "\(count) tools"
    }
}

/// Opens the rename prompt for the row it sits in.
private struct RenameButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "pencil")
                .foregroundStyle(.secondary)
                .frame(width: 24, height: 24)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Rename")
        .accessibilityLabel("Rename")
    }
}

/// A client on this Mac, and whether Plug is wired into it.
private struct AppLinkRow: View {
    let app: LinkableApp
    /// The name to show: the owner's, else the app's own.
    let name: String
    /// Open sessions counted from the live snapshot, which is fresher than
    /// the app scan.
    let sessionCount: Int
    /// Set when the row has sessions to show underneath.
    let isExpanded: Binding<Bool>?
    let isBusy: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.snug) {
            AppGlyph(target: app.target, name: app.name)
                .opacity(app.detected || app.linked || sessionCount > 0 ? 1 : 0.4)
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(name)
                    .font(.body)
                    .foregroundStyle(app.detected || app.linked ? .primary : .secondary)
                Label(status, systemImage: statusSymbol)
                    .font(.caption)
                    .foregroundStyle(isLive ? Color.green : Color.secondary)
                    .labelStyle(.titleAndIcon)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            if let isExpanded, sessionCount > 0 {
                Button {
                    isExpanded.wrappedValue.toggle()
                } label: {
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .rotationEffect(.degrees(isExpanded.wrappedValue ? 90 : 0))
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(isExpanded.wrappedValue ? "Hide sessions" : "Show sessions")
                .accessibilityLabel(isExpanded.wrappedValue ? "Hide sessions" : "Show sessions")
            }
            if isBusy {
                ProgressView().controlSize(.small)
            } else if app.detected || app.linked {
                Toggle(
                    "Use Plug",
                    isOn: Binding(
                        get: { app.linked },
                        set: { run($0 ? .linkApp(app.target) : .unlinkApp(app.target)) }
                    )
                )
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
                .disabled(!app.detected && !app.linked)
            }
        }
        .padding(.vertical, Metric.tight)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(name), \(status)")
        // Combining the row hides its switch from VoiceOver.
        .accessibilityActions {
            if let isExpanded, sessionCount > 0 {
                Button(isExpanded.wrappedValue ? "Hide Sessions" : "Show Sessions") {
                    isExpanded.wrappedValue.toggle()
                }
            }
            if !isBusy, app.detected || app.linked {
                Button(app.linked ? "Stop Using Plug" : "Use Plug") {
                    run(app.linked ? .unlinkApp(app.target) : .linkApp(app.target))
                }
            }
        }
    }

    /// The state as a glyph, so linked and not-linked are told apart before
    /// the sentence is read.
    private var isLive: Bool { sessionCount > 0 }

    private var statusSymbol: String {
        if isLive { return "bolt.fill" }
        guard app.linked else { return app.detected ? "circle" : "questionmark.app.dashed" }
        return "checkmark.circle"
    }

    private var status: String {
        if isLive {
            let count = sessionCount
            return count == 1 ? "1 session" : "\(count) sessions"
        }
        guard app.linked else { return app.detected ? "Not using Plug" : "Not installed" }
        if !app.detected { return "Set up · app not found" }
        return app.transport?.lowercased() == "http" ? "Ready · over the network" : "Ready"
    }
}

private struct GrantRow: View {
    let grant: DownstreamClient
    /// The name to show: the owner's, else the one the client registered.
    let name: String
    /// Asks for a new name, starting from the one shown.
    let rename: (String) -> Void
    let run: (PlugIntent) -> Void
    @State private var confirming = false

    var body: some View {
        HStack(spacing: Metric.snug) {
            Image(systemName: "key.horizontal")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(name).font(.body)
                Text(grantDetail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            RenameButton { rename(name) }
            Button("Revoke…", role: .destructive) { confirming = true }
                .controlSize(.small)
        }
        .padding(.vertical, Metric.tight)
        .confirmationDialog(
            "Revoke \(name)?",
            isPresented: $confirming,
            titleVisibility: .visible
        ) {
            Button("Revoke Access", role: .destructive) { run(.revokeClient(id: grant.clientId)) }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("It loses access immediately and has to ask for permission again.")
        }
    }

    private var grantDetail: String {
        // A client that identifies itself by web address is named by that
        // site. One Plug registered gets a short id, since two can share a
        // name. The registration method is not shown: nobody can act on it.
        if let host = URL(string: grant.clientId)?.host() { return host }
        let id = grant.clientId.hasPrefix("plug_") ? grant.clientId.dropFirst(5) : Substring(grant.clientId)
        return "ID \(id.prefix(8))"
    }
}
