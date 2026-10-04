import PlugIPC
import SwiftUI

/// The workbench. Servers that need something come first, because that is why
/// the window is open; everything healthy sits underneath, quiet.
struct ServersView: View {
    let model: AppModel
    @Bindable var router: Router
    @Binding var search: String
    let run: (PlugIntent) -> Void
    /// The server being removed, while the app asks first.
    @State private var removing: String?

    var body: some View {
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading servers…")
            } else if model.initialDataUnavailable {
                UnavailablePage(item: "Servers") { run(.reconnect) }
            } else if model.situation.servers.isEmpty {
                EmptyPage(
                    title: "No servers yet",
                    message: "Add one and every client connected to Plug can use it right away.",
                    symbol: "shippingbox",
                    actionTitle: "Add Server",
                    actionIntent: .addServer,
                    secondaryTitle: "Import Servers…",
                    secondaryIntent: .importServers,
                    run: run
                )
            } else if visibleNames.isEmpty {
                ContentUnavailableView.search(text: search)
            } else {
                list
            }
        }
        .navigationSubtitle(serverSummary ?? "")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button { run(.addServer) } label: {
                    Label("Add Server", systemImage: "plus")
                }
                .help("Add a server")

                Button { run(.importServers) } label: {
                    Label("Import Servers", systemImage: "square.and.arrow.down")
                }
                .help("Import servers from other clients")
            }
        }
        .onChange(of: visibleNames, initial: true) { keepSelectionVisible() }
        .confirmationDialog(
            "Remove \(removing ?? "")?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible,
            presenting: removing
        ) { name in
            Button("Remove Server", role: .destructive) { run(.removeServer(name)) }
            Button("Cancel", role: .cancel) { }
        } message: { _ in
            Text("Clients connected to Plug will stop seeing its tools. Your settings file keeps everything else.")
        }
    }

    private var selected: ServerFacts? {
        model.situation.servers.first { $0.name == router.selectedServer }
    }

    /// Servers on the left, the selected one in full on the right. A server is
    /// always selected, so the right side is never an empty placeholder.
    private var list: some View {
        ListDetail {
            List(selection: $router.selectedServer) {
                group("Needs Attention", servers: matching(model.situation.troubledServers))
                group("Starting", servers: matching(startingServers))
                group("Running", servers: matching(runningServers))
                group("Off", servers: matching(offServers))
            }
            .onDeleteCommand {
                if model.canMutate { removing = router.selectedServer }
            }
        } detail: {
            if let selected {
                ServerDetailView(
                    model: model,
                    server: selected,
                    query: search,
                    router: router,
                    run: run
                )
                .id(selected.name)
            } else {
                NoSelection(item: "Server")
            }
        }
    }

    private var visibleNames: [String] {
        (matching(model.situation.troubledServers)
            + matching(startingServers)
            + matching(runningServers)
            + matching(offServers)).map(\.name)
    }

    /// NSTableView is still finishing its own update when the list changes, so
    /// the selection moves on the next main-actor turn.
    private func keepSelectionVisible() {
        let names = visibleNames
        if let current = router.selectedServer, names.contains(current) { return }
        Task { @MainActor in
            await Task.yield()
            router.selectedServer = names.first
        }
    }

    @ViewBuilder
    private func group(_ title: String, servers: [ServerFacts]) -> some View {
        if !servers.isEmpty {
            Section(title) {
                ForEach(servers) { server in
                    ServerListRow(server: server)
                        .tag(server.name)
                        .contextMenu {
                            ServerActions(server: server, run: run)
                            Divider()
                            Button("Remove Server…", role: .destructive) { removing = server.name }
                        }
                }
            }
        }
    }

    private var serverSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        return model.situation.countsSummary(stale: model.dataIsStale)
    }

    private var runningServers: [ServerFacts] {
        model.situation.activeServers.filter { $0.health == .working }
    }

    private var startingServers: [ServerFacts] {
        model.situation.activeServers.filter { $0.health == .starting }
    }

    private var offServers: [ServerFacts] {
        model.situation.servers.filter { !$0.enabled }
    }

    /// A search keeps a server whose name matches, and one that has a tool
    /// matching, so a tool can be found without knowing which server has it.
    private func matching(_ servers: [ServerFacts]) -> [ServerFacts] {
        let query = search.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return servers }
        let withTools = model.toolCatalog.servers(withToolsMatching: query)
        return servers.filter {
            $0.name.localizedCaseInsensitiveContains(query) || withTools.contains($0.name)
        }
    }
}

/// A server as a row: one glyph for its state, its name, and what it offers.
/// The group it sits in already says whether it is running, so the row does
/// not say it again.
private struct ServerListRow: View {
    let server: ServerFacts

    var body: some View {
        HStack(spacing: Metric.tight) {
            StatusGlyph(health: server.health)
            Text(server.name)
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(server.enabled ? .primary : .secondary)
            Spacer(minLength: Metric.tight)
            if let trailing {
                Text(trailing)
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .layoutPriority(1)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            server.health == .working
                ? "\(server.name), \(server.health.label), \(server.toolCountText)"
                : "\(server.name), \(server.health.label)"
        )
    }

    private var trailing: String? {
        if server.health == .working { return "\(server.toolCount)" }
        if server.health.needsAttention { return server.health.label }
        return nil
    }
}

/// The same verbs everywhere a server can be acted on.
struct ServerActions: View {
    let server: ServerFacts
    let run: (PlugIntent) -> Void

    var body: some View {
        if server.health == .signInNeeded {
            Button("Sign In…") { run(.signIn(server: server.name)) }
        }
        if server.enabled {
            Button("Restart") { run(.restartServer(server.name)) }
            Button("Turn Off") { run(.setServerEnabled(server.name, false)) }
        } else {
            Button("Turn On") { run(.setServerEnabled(server.name, true)) }
        }
        Button("Edit…") { run(.editServer(server.name)) }
    }
}
