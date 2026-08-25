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
                        if !server.authenticated {
                            Button(model.signingInServers.contains(server.name) ? "Waiting for browser…" : "Sign In") {
                                Task { await model.signIn(server: server.name) }
                            }
                            .disabled(model.signingInServers.contains(server.name))
                        }
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
