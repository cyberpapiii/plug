import PlugIPC
import SwiftUI

struct ServersView: View {
    let model: AppModel
    @State private var selection: ServerStatus.ID?
    @State private var showingAdd = false

    var body: some View {
        HSplitView {
            List(model.visibleServers, selection: $selection) { server in
                HStack(spacing: 10) {
                    Circle().fill(server.health == "Healthy" ? .green : .orange).frame(width: 8, height: 8)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(server.serverID)
                        Text("\(server.toolCount) tools · \(server.health)").font(.caption).foregroundStyle(.secondary)
                    }
                }.tag(server.id)
            }.frame(minWidth: 260)
            if let server = model.visibleServers.first(where: { $0.id == selection }) {
                ServerDetailView(model: model, server: server)
            } else {
                ContentUnavailableView("Choose a server", systemImage: "shippingbox")
            }
        }
        .navigationTitle("Servers")
        .toolbar { Button { showingAdd = true } label: { Label("Add Server", systemImage: "plus") } }
        .sheet(isPresented: $showingAdd) { AddServerView(model: model) }
    }
}

struct ServerDetailView: View {
    let model: AppModel
    let server: ServerStatus
    var body: some View {
        Form {
            LabeledContent("Status", value: server.health)
            LabeledContent("Tools", value: String(server.toolCount))
            HStack {
                Button("Restart") { Task { await model.perform { .restartServer(authToken: $0, serverID: server.serverID) } } }
                Button("Disable") { Task { await model.perform { .setServerEnabled(authToken: $0, name: server.serverID, enabled: false) } } }
                Spacer()
                Button("Remove", role: .destructive) { Task { await model.perform { .removeServer(authToken: $0, name: server.serverID) } } }
            }
        }.formStyle(.grouped).padding().navigationTitle(server.serverID)
    }
}

struct AddServerView: View {
    let model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var definition = ""
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Add Server").font(.title2.bold())
            TextField("Name", text: $name)
            TextField("Paste a command or URL", text: $definition)
            Text("Plug validates it before changing your setup.").font(.caption).foregroundStyle(.secondary)
            HStack { Spacer(); Button("Cancel") { dismiss() }; Button("Add") { add() }.keyboardShortcut(.defaultAction).disabled(name.isEmpty || definition.isEmpty || working) }
        }.padding(24).frame(width: 480)
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
