import PlugIPC
import SwiftUI

/// The workbench. Servers that need something come first, because that is why
/// the window is open; everything healthy sits underneath, quiet.
struct ServersView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    @State private var search = ""

    var body: some View {
        Group {
            if model.situation.servers.isEmpty {
                EmptyPage(
                    title: "No servers yet",
                    message: "Add one and every AI app connected to Plug can use it right away.",
                    symbol: "shippingbox",
                    actionTitle: "Add Server",
                    actionIntent: .addServer,
                    run: run
                )
            } else {
                list
            }
        }
        .searchable(text: $search, placement: .toolbar, prompt: "Search servers")
        .toolbar {
            ToolbarItem {
                Button { run(.addServer) } label: {
                    Label("Add Server", systemImage: "plus")
                }
                .keyboardShortcut("n", modifiers: .command)
                .help("Add a server")
            }
        }
        .inspector(isPresented: inspectorShown) {
            if let selected {
                ServerDetailView(model: model, server: selected, router: router, run: run)
                    .inspectorColumnWidth(min: 280, ideal: 320, max: 380)
            }
        }
        .sheet(isPresented: $router.isAddingServer) {
            AddServerView(model: model)
        }
    }

    /// The inspector is open exactly when a server is selected, and closing it
    /// clears the selection, so the two can never disagree.
    private var inspectorShown: Binding<Bool> {
        Binding(
            get: { selected != nil },
            set: { shown in if !shown { router.selectedServer = nil } }
        )
    }

    private var selected: ServerFacts? {
        model.situation.servers.first { $0.name == router.selectedServer }
    }

    private var list: some View {
        List(selection: $router.selectedServer) {
            group("Needs attention", servers: matching(model.situation.troubledServers))
            group("Running", servers: matching(runningServers))
            group("Off", servers: matching(offServers))
        }
        .listStyle(.inset)
        .overlay {
            if matching(model.situation.servers).isEmpty {
                ContentUnavailableView.search(text: search)
            }
        }
    }

    @ViewBuilder
    private func group(_ title: String, servers: [ServerFacts]) -> some View {
        if !servers.isEmpty {
            Section(title) {
                ForEach(servers) { server in
                    ServerListRow(server: server, run: run)
                        .tag(server.name)
                        .contextMenu { ServerActions(server: server, run: run) }
                }
            }
        }
    }

    private var runningServers: [ServerFacts] {
        model.situation.activeServers.filter { !$0.health.needsAttention }
    }

    private var offServers: [ServerFacts] {
        model.situation.servers.filter { !$0.enabled }
    }

    private func matching(_ servers: [ServerFacts]) -> [ServerFacts] {
        let query = search.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return servers }
        return servers.filter { $0.name.localizedCaseInsensitiveContains(query) }
    }
}

/// A server as a row: state, name, what it offers, and — when something is
/// wrong — the button that fixes it, without opening anything first.
private struct ServerListRow: View {
    let server: ServerFacts
    let run: (PlugIntent) -> Void
    @State private var hovering = false

    var body: some View {
        HStack(spacing: Metric.snug) {
            StatusGlyph(health: server.health)
            VStack(alignment: .leading, spacing: 0) {
                Text(server.name).font(.body)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: Metric.tight)
            if let fix {
                Button(fix.title) { run(fix.intent) }
                    .controlSize(.small)
            } else if server.health == .working {
                Text(server.toolCount == 1 ? "1 tool" : "\(server.toolCount) tools")
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(hovering ? .secondary : .tertiary)
            }
        }
        .padding(.vertical, Metric.tight - 2)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }

    private var subtitle: String {
        if server.health.needsAttention, let error = server.error, !error.isEmpty {
            return error.split(separator: "\n").first.map(String.init) ?? error
        }
        if !server.enabled { return "Switched off" }
        return transportLabel
    }

    private var transportLabel: String {
        switch server.transport.lowercased() {
        case "stdio": "Runs on this Mac"
        case "sse": "Remote server"
        case "http": "Remote server"
        default: server.transport.capitalized
        }
    }

    private var fix: Verdict.Button? {
        switch server.health {
        case .signInNeeded: server.isSigningIn ? nil : .init("Sign In", .signIn(server: server.name))
        case .down, .unknown: .init("Restart", .restartServer(server.name))
        default: nil
        }
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
    }
}
