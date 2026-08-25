import PlugIPC
import SwiftUI

struct ServersView: View {
    let model: AppModel
    @State private var selection: ServerStatus.ID?
    @State private var showingInspector = false
    @State private var showingAdd = false

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(
                title: "Servers",
                subtitle: serverSummary,
                metrics: [
                    (String(enabledCount), "Enabled"),
                    (String(totalTools), "Tools")
                ]
            )

            List(model.visibleServers, selection: $selection) { server in
                ServerRow(server: server)
                    .tag(server.id)
                    .contextMenu { ServerContextMenu(model: model, server: server) }
            }
            .listStyle(.inset)
            .overlay {
                if model.visibleServers.isEmpty {
                    ContentUnavailableView(
                        "No servers yet",
                        systemImage: "shippingbox",
                        description: Text("Add a command or URL to get started.")
                    )
                }
            }
        }
        .navigationTitle("Servers")
        .toolbar {
            Button { showingAdd = true } label: {
                Label("Add Server", systemImage: "plus")
            }
            .keyboardShortcut("n", modifiers: .command)
        }
        .onChange(of: selection) { _, value in showingInspector = value != nil }
        .inspector(isPresented: $showingInspector) {
            if let server = selectedServer {
                ServerInspector(model: model, server: server, isPresented: $showingInspector)
                    .inspectorColumnWidth(min: 260, ideal: 290, max: 340)
            }
        }
        .sheet(isPresented: $showingAdd) { AddServerView(model: model) }
    }

    private var selectedServer: AppModel.ServerPresentation? {
        model.visibleServers.first { $0.id == selection }
    }

    private var enabledCount: Int {
        model.visibleServers.filter(\.configured.enabled).count
    }

    private var totalTools: Int {
        model.visibleServers.reduce(0) { $0 + $1.toolCount }
    }

    private var serverSummary: String {
        let attention = model.visibleServers.filter {
            $0.configured.enabled && $0.health != "Healthy"
        }.count
        return attention == 0 ? "Everything is running normally" : "\(attention) need attention"
    }
}

private struct ServerRow: View {
    let server: AppModel.ServerPresentation

    var body: some View {
        HStack(spacing: 12) {
            StatusDot(color: server.statusColor)
            VStack(alignment: .leading, spacing: 2) {
                Text(server.configured.name).fontWeight(.medium)
                Text(server.configured.transport.capitalized)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Text("\(server.toolCount) tools")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(minWidth: 72, alignment: .trailing)
            Text(server.displayHealth)
                .font(.callout)
                .foregroundStyle(server.health == "Healthy" ? .secondary : server.statusColor)
                .frame(minWidth: 92, alignment: .leading)
        }
        .padding(.vertical, 6)
        .contentShape(Rectangle())
        .help(server.runtime?.error ?? server.displayHealth)
    }
}

private struct ServerInspector: View {
    let model: AppModel
    let server: AppModel.ServerPresentation
    @Binding var isPresented: Bool
    @State private var confirmRemoval = false

    var body: some View {
        Form {
            Section {
                HStack(spacing: 10) {
                    StatusDot(color: server.statusColor)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(server.configured.name).font(.headline)
                        Text(server.displayHealth).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button { isPresented = false } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.tertiary)
                    }
                    .buttonStyle(.plain)
                    .help("Close Details")
                }
            }
            Section("Details") {
                LabeledContent("Transport", value: server.configured.transport.capitalized)
                LabeledContent("Tools", value: String(server.toolCount))
                LabeledContent("Authentication", value: server.configured.oauth ? "OAuth" : "None")
            }
            if let error = server.runtime?.error, !error.isEmpty {
                Section("Last error") {
                    Text(error).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
                }
            }
            Section {
                if server.configured.enabled {
                    Button("Restart Server") {
                        Task { await model.perform { .restartServer(authToken: $0, serverID: server.id) } }
                    }
                    Button("Disable Server") {
                        Task { await model.perform { .setServerEnabled(authToken: $0, name: server.id, enabled: false) } }
                    }
                } else {
                    Button("Enable Server") {
                        Task { await model.perform { .setServerEnabled(authToken: $0, name: server.id, enabled: true) } }
                    }
                }
                Button("Remove Server…", role: .destructive) { confirmRemoval = true }
            }
        }
        .formStyle(.grouped)
        .confirmationDialog(
            "Remove \(server.configured.name)?",
            isPresented: $confirmRemoval
        ) {
            Button("Remove Server", role: .destructive) {
                Task { await model.perform { .removeServer(authToken: $0, name: server.id) } }
            }
        } message: {
            Text("Plug will remove this server from your configuration.")
        }
    }
}

private struct ServerContextMenu: View {
    let model: AppModel
    let server: AppModel.ServerPresentation

    var body: some View {
        if server.configured.enabled {
            Button("Restart") {
                Task { await model.perform { .restartServer(authToken: $0, serverID: server.id) } }
            }
            Button("Disable") {
                Task { await model.perform { .setServerEnabled(authToken: $0, name: server.id, enabled: false) } }
            }
        } else {
            Button("Enable") {
                Task { await model.perform { .setServerEnabled(authToken: $0, name: server.id, enabled: true) } }
            }
        }
    }
}

struct AddServerView: View {
    let model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var definition = ""
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Add a server").font(.title2.weight(.semibold))
                Text("Paste the command you run or the server URL.")
                    .foregroundStyle(.secondary)
            }
            Form {
                TextField("Name", text: $name, prompt: Text("My server"))
                TextField("Command or URL", text: $definition, prompt: Text("npx server or https://…"))
            }
            .formStyle(.grouped)
            HStack {
                Text("Plug validates this before saving.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Cancel") { dismiss() }
                Button(working ? "Adding…" : "Add Server") { add() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(name.isEmpty || definition.isEmpty || working)
            }
        }
        .padding(24)
        .frame(width: 520)
    }

    private func add() {
        working = true
        let server: ServerConfig
        if definition.hasPrefix("http://") || definition.hasPrefix("https://") {
            server = .remote(definition)
        } else {
            let parts = definition.split(whereSeparator: \.isWhitespace).map(String.init)
            server = .command(parts.first ?? definition, args: Array(parts.dropFirst()))
        }
        Task {
            await model.perform { .validateServer(authToken: $0, name: name, server: server) }
            guard model.lastError == nil else { working = false; return }
            await model.perform { .addServer(authToken: $0, name: name, server: server) }
            working = false
            if model.lastError == nil { dismiss() }
        }
    }
}

private extension AppModel.ServerPresentation {
    var displayHealth: String {
        switch health {
        case "AuthRequired": "Sign in required"
        case "Starting": "Starting"
        case "Failed": "Unavailable"
        default: health
        }
    }

    var statusColor: Color {
        switch health {
        case "Healthy": .green
        case "Disabled": .secondary
        case "Failed": .red
        default: .orange
        }
    }
}
