import SwiftUI

struct AuthView: View {
    let model: AppModel
    var body: some View {
        List {
            Section("Upstream servers") {
                ForEach(model.snapshot.upstreamAuth) { server in
                    HStack {
                        VStack(alignment: .leading) { Text(server.name); Text(server.authenticated ? "Connected" : "Needs sign-in").font(.caption).foregroundStyle(.secondary) }
                        Spacer()
                        if !server.authenticated { Text("Sign in from Plug CLI").foregroundStyle(.orange) }
                    }
                }
                if model.snapshot.upstreamAuth.isEmpty { Text("No OAuth servers configured").foregroundStyle(.secondary) }
            }
            Section("Remote clients") {
                ForEach(model.snapshot.downstreamClients) { client in
                    HStack { Text(client.clientName); Spacer(); Button("Revoke", role: .destructive) { Task { await model.perform { .revokeClient(authToken: $0, clientID: client.clientID) } } } }
                }
                if model.snapshot.downstreamClients.isEmpty { Text("No remote grants").foregroundStyle(.secondary) }
            }
        }.navigationTitle("Auth")
    }
}
