import PlugIPC
import SwiftUI

/// The workbench. Servers that need something come first, because that is why
/// the window is open; everything healthy sits underneath, quiet.
struct ServersView: View {
    let model: AppModel
    @Bindable var router: Router
    @Binding var search: String
    let run: (PlugIntent) -> Void

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "Servers", detail: serverSummary) {
                HStack(spacing: Metric.tight) {
                    Button { run(.addServer) } label: {
                        Label("Add Server", systemImage: "plus")
                    }
                    .keyboardShortcut("n", modifiers: .command)
                    .help("Add a server")

                    Button { run(.importServers) } label: {
                        Image(systemName: "square.and.arrow.down")
                    }
                    .help("Import servers from other clients")
                    .accessibilityLabel("Import servers from other clients")
                }
                .disabled(!model.canMutate)
            }

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
                } else {
                    list
                }
            }
        }
        .onChange(of: visibleNames, initial: true) { keepSelectionVisible() }
        .sheet(isPresented: $router.isAddingServer) {
            AddServerView(model: model)
        }
        .sheet(isPresented: $router.isImportingServers) {
            ImportServersView(model: model)
        }
        .sheet(item: $router.editingServer) { target in
            EditServerView(model: model, name: target.id)
        }
        .sheet(item: $router.addingAccountTo) { target in
            AddAccountView(model: model, router: router, server: target.id)
        }
    }

    private var selected: ServerFacts? {
        model.situation.servers.first { $0.name == router.selectedServer }
    }

    /// Servers on the left, the selected one in full on the right. A server is
    /// always selected, so the right side is never an empty placeholder.
    @ViewBuilder private var list: some View {
        if visibleNames.isEmpty {
            ContentUnavailableView.search(text: search)
        } else {
            HStack(spacing: 0) {
                List(selection: $router.selectedServer) {
                    group("Needs attention", servers: matching(model.situation.troubledServers))
                    group("Starting", servers: matching(startingServers))
                    group("Running", servers: matching(runningServers))
                    group("Off", servers: matching(offServers))
                }
                .listStyle(.inset)
                .frame(width: Metric.serverListWidth)
                Divider()
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
                    Color.clear
                }
            }
            .frame(maxWidth: Metric.contentMaxWidth)
            .frame(maxWidth: .infinity)
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
            SectionLabel(
                text: title,
                trailing: servers.count == 1 ? "1 server" : "\(servers.count) servers"
            )
                .padding(.top, Metric.regular)
                .listRowSeparator(.hidden)
                .listRowBackground(Color.clear)
            ForEach(servers) { server in
                ServerListRow(server: server)
                    .tag(server.name)
                    .listRowSeparator(.hidden)
                    .listRowInsets(Metric.listRowInsets)
                    .contextMenu { ServerActions(server: server, run: run) }
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

/// A server as a row: state, name, and what it offers. The fix for a
/// troubled server is in the details beside the list.
private struct ServerListRow: View {
    let server: ServerFacts

    var body: some View {
        HStack(spacing: Metric.snug) {
            StatusGlyph(health: server.health)
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(server.name).font(.callout.weight(.medium))
                Label(subtitle, systemImage: server.subtitleSymbol)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: Metric.tight)
            if server.health == .working {
                Text(server.toolCountText)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, Metric.tight)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var subtitle: String {
        if server.health.needsAttention { return server.health.label }
        if !server.enabled { return server.health.label }
        return server.transportLabel
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
