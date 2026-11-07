import PlugIPC
import SwiftUI

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
    @Environment(\.windowSeen) private var windowSeen

    private var sessions: [LiveSession] { model.snapshot.liveSessions }
    private var allApps: [LinkableApp] {
        model.actions.connectableApps.filter { $0.detected || $0.linked || $0.live }
    }
    /// Every app is placed against all sessions, then search narrows what
    /// shows, so a session never leaves its app's row because the app was
    /// filtered out.
    ///
    /// The body builds this once for each time it is drawn and hands it to
    /// whatever needs it.
    private func roster(of apps: [LinkableApp]) -> AppRoster {
        let names = self.names
        return AppRoster(apps: apps, sessions: sessions) {
            names.knownTarget(named: names.name(forKey: $0.key))
        }
    }
    /// Kept by the model and built once per snapshot. Every row asks it
    /// about every session.
    private var names: ClientNames { model.clientNames }
    private var grants: [DownstreamClient] {
        model.snapshot.downstreamClients.filter {
            matches(names.name(forKey: $0.clientKey) ?? "") || matches($0.clientName)
                || matches($0.source) || matches($0.clientId)
        }
    }

    /// The open sessions no app or sign-in claimed, sorted into the clients
    /// they belong to, so one client is one row however many connections it
    /// has open.
    private func unclaimed(in roster: AppRoster) -> [(id: String, sessions: [LiveSession])] {
        let names = self.names
        let grantKeys = Set(model.snapshot.downstreamClients.map(\.clientKey))
        var result: [(id: String, sessions: [LiveSession])] = []
        var index: [String: Int] = [:]
        for session in roster.other {
            if let key = session.key, grantKeys.contains(key) { continue }
            let id = "session:\(names.client(of: session))"
            if let at = index[id] {
                result[at].sessions.append(session)
            } else {
                index[id] = result.count
                result.append((id: id, sessions: [session]))
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

    private func access(key: String, name: String, isRemote: Bool = false) -> ClientAccess {
        ClientAccess(
            key: key,
            name: name,
            isRemote: isRemote,
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
        let overNetwork = app.overNetwork
            || sessions.contains { $0.ownAccessKey != app.target }
        guard !overNetwork else { return nil }
        return access(key: app.target, name: names.name(forKey: app.target) ?? app.name)
    }

    var body: some View {
        let apps = allApps
        let roster = self.roster(of: apps)
        let unclaimed = self.unclaimed(in: roster)
        let entries = self.entries(apps: apps, roster: roster, unclaimed: unclaimed)
        return SectionPage(
            model: model,
            loading: "Loading clients",
            isEmpty: entries.isEmpty,
            search: search,
            run: run
        ) {
            if model.actions.isLoadingConnectableApps {
                LoadingPage(message: "Loading clients")
            } else if let error = model.actions.connectableAppsError {
                PagePane {
                    ContentUnavailableView {
                        PlugCharacterLabel(title: "Clients Unavailable", mood: .worried)
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try Again") { Task { await model.actions.loadConnectableApps() } }
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
        } content: {
            ListDetail {
                List(selection: $router.selectedClient) {
                    if let error = model.actions.connectableAppsError {
                        ProblemNote(title: "Plug could not look for clients on this Mac", reason: error, failed: true)
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
        .pageSubtitle(connectionSummary)
        .toolbar {
            // The window draws a section once per column; the button sits
            // beside search. Above the list it did not fit once the sidebar
            // was hidden, and the bar moved it about.
            if pane != .list {
                ToolbarItem(placement: .primaryAction) {
                    Button { run(.addClient) } label: {
                        Label("Add Client", icon: .add)
                    }
                    .help("Add a client")
                }
            }
        }
        .onChange(of: entries.map(\.id), initial: true) { _, ids in keepSelectionVisible(ids) }
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
        // Not while the window is covered or minimized: no one is reading it.
        .task(id: windowSeen) {
            guard windowSeen else { return }
            while !Task.isCancelled {
                await model.actions.loadConnectableApps()
                try? await Task.sleep(for: .seconds(5), tolerance: .seconds(1))
            }
        }
    }

    /// NSTableView is still finishing its own update when the list changes, so
    /// the selection moves on the next main-actor turn.
    private func keepSelectionVisible(_ ids: [String]) {
        if let current = router.selectedClient, ids.contains(current) { return }
        Task { @MainActor in
            await Task.yield()
            router.selectedClient = ids.first
        }
    }

    @ViewBuilder
    private func group(_ place: String, in entries: [ClientEntry]) -> some View {
        let rows = entries.filter { $0.place == place }
        if !rows.isEmpty {
            ListGroupHeader(place)
            ForEach(rows) { entry in
                ClientRow(entry: entry, face: entry.face, canMutate: model.canMutate)
                    .equatable()
                    .tag(entry.id)
            }
            .listRowSeparator(.hidden)
        }
    }

    // MARK: - Entries

    /// Every client the list shows, in the order it shows them. The list and
    /// the detail beside it both read from here, so they cannot disagree.
    private func entries(apps allApps: [LinkableApp], roster: AppRoster, unclaimed: [(id: String, sessions: [LiveSession])]) -> [ClientEntry] {
        let names = self.names
        let sessions = self.sessions
        let others = unclaimed
            .filter { client in
                client.sessions.contains {
                    matches(displayName($0)) || matches($0.transport) || matches($0.sessionId)
                }
            }
            .map { sessionEntry(id: $0.id, sessions: $0.sessions) }
        let represented = Set(allApps.filter { app in
            ClientStatus.hasNetworkRepresentation(
                app, grantTargets: model.snapshot.downstreamClients.map(names.target(ofGrant:))
            )
        }.map(\.target))
        let apps = roster.connected
            .filter { matches($0.app.name) || matches($0.app.target) }
            .filter { !represented.contains($0.app.target) }
            .map { appEntry($0.app, sessions: $0.sessions) }
        let idle = roster.idle
            .filter { matches($0.name) || matches($0.target) }
            .filter { !represented.contains($0.target) }
            .map { appEntry($0, sessions: []) }
        let granted = grants.map { grant in
            let target = names.target(ofGrant: grant)
            var own = sessions.filter { $0.key == grant.clientKey }
            // A uniquely identified grant can show its still-open local
            // sessions too. Multiple grants remain separate identities.
            if represented.contains(target), model.snapshot.downstreamClients.filter({
                names.target(ofGrant: $0) == target
            }).count == 1 {
                own += sessions.filter { !$0.isRemote && AppRoster.targets(of: $0).contains(target) }
            }
            return grantEntry(grant, sessions: own)
        }
        // A client that connects, calls, and is gone again has no row of
        // its own above. Its last call puts it here.
        let shown = Set((apps + idle + others + granted).flatMap { [$0.renameKey, $0.placeKey].compactMap { $0 } })
            .union(allApps.map(\.target))
            .union(sessions.compactMap(\.key))
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
    private func appEntry(_ app: LinkableApp, sessions: [LiveSession]) -> ClientEntry {
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
        return ClientEntry(
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
            isBusy: model.actions.busyApps.contains(app.target),
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
    private func sessionEntry(id: String, sessions: [LiveSession]) -> ClientEntry {
        let first = sessions[0]
        let name = displayName(first)
        let choices = first.ownAccessKey.map { access(key: $0, name: name, isRemote: first.isRemote) }
        return ClientEntry(
            id: id,
            group: first.isRemote ? .network : .onThisMac,
            place: ClientPlaces.place(
                owners: names.place(forKey: first.key),
                group: first.isRemote ? .network : .onThisMac
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
            renameKey: first.key,
            placeKey: first.key,
            disambiguator: String(first.sessionId.prefix(4))
        )
    }

    /// A client that is not connected now and has no entry of its own, by
    /// the last call it made. A script that runs on a timer is one.
    private func recentEntry(_ event: ActivityEvent) -> ClientEntry {
        let call = model.call(event)
        let key = event.clientKey ?? ""
        let group: ClientEntry.Group = event.remote ? .network : .onThisMac
        // Every remote client without a sign-in of its own shares one key,
        // so there are no choices to offer for one of them.
        let choices = key == ActivityEvent.sharedRemoteKey
            ? nil
            : access(key: key, name: call.caller, isRemote: event.remote)
        return ClientEntry(
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
            glyph: .app(target: call.callerTarget, name: call.caller, appPath: nil),
            isBusy: false,
            switchLabel: "",
            isOn: nil,
            setOn: { _ in },
            about: "Not connected now. This client connects when it has something to do and leaves when it is done, so Plug lists it by its last call. It stays here while that call is in Activity.",
            access: choices,
            // Named and placed under the key its calls carry, as the row of
            // the same client is while it is connected.
            renameKey: key,
            placeKey: key
        )
    }

    /// A client allowed in over the network, with the sessions it has open.
    /// Its switch is its permission: off takes the permission away, and the
    /// client has to ask again, so the app asks first.
    private func grantEntry(_ grant: DownstreamClient, sessions: [LiveSession]) -> ClientEntry {
        let owned = names.name(forKey: grant.clientKey)
        let name = owned ?? grant.clientName
        if grant.isUnfinished { return unfinishedEntry(grant, name: name) }
        let access = access(key: grant.clientKey, name: name, isRemote: true)
        let host = URL(string: grant.clientId)?.host()
        let since = SignInFacts.day(grant.createdAt)
        // A client with a connection open is plainly signed in.
        let needsSignIn = grant.needsSignIn && sessions.isEmpty
        let target = names.target(ofGrant: grant)
        let earlier = SignInFacts.earlier(
            than: grant, among: model.snapshot.downstreamClients, ownerNamed: owned != nil, answered: answered
        )
        return ClientEntry(
            id: ClientEntry.id(grant: grant.clientId),
            group: .network,
            place: ClientPlaces.place(owners: names.place(forKey: grant.clientKey), group: .network),
            name: name,
            originalName: grant.clientName,
            status: ClientStatus.grant(
                connections: sessions.count, needsSignIn: grant.needsSignIn, limit: access.summary
            ),
            isLive: !sessions.isEmpty,
            dimmed: false,
            glyph: .grant(target: target, name: ClientIdentity.displayName(forTarget: target) ?? grant.clientName),
            isBusy: false,
            switchLabel: "Access",
            isOn: true,
            setOn: { allowed in
                if !allowed { revoking = Revoking(id: grant.clientId, name: name) }
            },
            about: needsSignIn
                ? ClientSignIn.about(target: target, name: name)
                : "Signed in \(since). Turn this off to remove its access.",
            access: access,
            renameKey: grant.clientKey,
            placeKey: grant.clientKey,
            disambiguator: host ?? (since == "today" ? since : "since \(since)"),
            grantID: grant.clientId,
            needsSignIn: needsSignIn,
            signInCommand: needsSignIn ? ClientSignIn.command(target: target) : nil,
            asking: earlier.isEmpty ? nil : ClientEntry.Asking(
                when: since,
                choices: earlier.map { other in
                    ClientEntry.Asking.Choice(
                        id: other.clientId,
                        name: names.name(forKey: other.clientKey)
                            ?? "\(other.clientName), since \(SignInFacts.day(other.createdAt))"
                    )
                },
                replace: { id in
                    if let old = earlier.first(where: { $0.clientId == id }) {
                        run(.replaceClient(old: old, new: grant))
                    }
                    answer(grant.clientId)
                },
                keep: { answer(grant.clientId) }
            )
        )
    }

    /// Something asked to be let in and nobody finished the sign-in. It has
    /// no access, so there is nothing to choose for it; its switch forgets
    /// it now.
    private func unfinishedEntry(_ grant: DownstreamClient, name: String) -> ClientEntry {
        let until = Date(timeIntervalSince1970: TimeInterval(grant.expiresAt))
            .formatted(date: .omitted, time: .shortened)
        return ClientEntry(
            id: ClientEntry.id(grant: grant.clientId),
            group: .unfinished,
            place: ClientEntry.Group.unfinished.rawValue,
            name: name,
            originalName: grant.clientName,
            status: ClientStatus(state: "Sign-in never finished", limit: nil),
            isLive: false,
            dimmed: true,
            glyph: .grant(target: names.target(ofGrant: grant), name: name),
            isBusy: false,
            switchLabel: "Access",
            isOn: nil,
            setOn: { _ in },
            about: "Something asked to use Plug under this name, and the sign-in was never finished. It cannot use anything. Plug forgets it at \(until).",
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

    /// Counted as the menu bar panel counts them.
    private var connectionSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = model.situation.connectedClients.count
        let summary = count == 0 ? "None connected" : "\(count) connected"
        return lastKnown(summary, stale: model.dataIsStale)
    }

    private func displayName(_ session: LiveSession) -> String { names.displayName(session) }

    private func sessionTarget(_ session: LiveSession) -> String { names.target(of: session) }
}
