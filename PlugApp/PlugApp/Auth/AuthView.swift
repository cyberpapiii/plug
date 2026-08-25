import PlugIPC
import SwiftUI

struct AuthView: View {
    let model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(
                title: "Authentication",
                subtitle: authSummary,
                metrics: [
                    (String(connectedCount), "Connected"),
                    (String(model.snapshot.downstreamClients.count), "Remote grants")
                ]
            )
            List {
                Section("Server accounts") {
                    if model.snapshot.upstreamAuth.isEmpty {
                        EmptySectionRow(title: "No OAuth servers configured", systemImage: "key")
                    }
                    ForEach(model.snapshot.upstreamAuth) { server in
                        HStack(spacing: 12) {
                            StatusDot(color: server.authenticated ? .green : .orange)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(server.name).fontWeight(.medium)
                                Text(server.authenticated ? expiryText(server) : "Sign in required")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if !server.authenticated {
                                Button(model.signingInServers.contains(server.name) ? "Waiting for browser…" : "Sign In") {
                                    Task { await model.signIn(server: server.name) }
                                }
                                .disabled(model.signingInServers.contains(server.name))
                            }
                        }
                        .padding(.vertical, 5)
                    }
                }
                Section("Remote clients") {
                    if model.snapshot.downstreamClients.isEmpty {
                        EmptySectionRow(title: "No remote grants", systemImage: "person.crop.circle.badge.checkmark")
                    }
                    ForEach(model.snapshot.downstreamClients) { client in
                        RemoteGrantRow(model: model, client: client)
                    }
                }
            }
            .listStyle(.inset)
        }
        .navigationTitle("Authentication")
    }

    private var connectedCount: Int {
        model.snapshot.upstreamAuth.filter(\.authenticated).count
    }

    private var authSummary: String {
        let missing = model.snapshot.upstreamAuth.count - connectedCount
        return missing == 0 ? "Accounts and remote access are in good shape" : "\(missing) server accounts need attention"
    }

    private func expiryText(_ server: PlugIPC.AuthServer) -> String {
        guard let seconds = server.tokenExpiresInSecs else { return "Connected" }
        if seconds < 3_600 { return "Connected · refreshes in under an hour" }
        if seconds >= 86_400 { return "Connected · refreshes in \(seconds / 86_400)d" }
        return "Connected · refreshes in \(seconds / 3_600)h"
    }
}

private struct RemoteGrantRow: View {
    let model: AppModel
    let client: DownstreamClient
    @State private var confirmRevocation = false

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "person.crop.circle.badge.checkmark")
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(client.clientName).fontWeight(.medium)
                Text("Authorized remote client")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Revoke…", role: .destructive) { confirmRevocation = true }
        }
        .padding(.vertical, 5)
        .confirmationDialog(
            "Revoke \(client.clientName)?",
            isPresented: $confirmRevocation
        ) {
            Button("Revoke Access", role: .destructive) {
                Task { await model.perform { .revokeClient(authToken: $0, clientID: client.clientId) } }
            }
        } message: {
            Text("This client will need to authorize again before it can use Plug.")
        }
    }
}
