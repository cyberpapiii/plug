import SwiftUI

struct ClientsView: View {
    let model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(
                title: "Clients",
                subtitle: clientSummary,
                metrics: [
                    (String(model.snapshot.liveSessions.count), "Connected"),
                    (String(model.snapshot.downstreamClients.count), "Remote grants")
                ]
            )
            List {
                Section("Connected now") {
                    if model.snapshot.liveSessions.isEmpty {
                        EmptySectionRow(title: "No clients connected", systemImage: "person.2")
                    }
                    ForEach(model.snapshot.liveSessions) { session in
                        HStack(spacing: 12) {
                            Image(systemName: clientSymbol(session.clientType))
                                .foregroundStyle(.secondary)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(session.clientInfo ?? displayName(session.clientType)).fontWeight(.medium)
                                Text(session.transport.replacingOccurrences(of: "_", with: " ").capitalized)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            VStack(alignment: .trailing, spacing: 2) {
                                Text("\(visibleCount(session.id)) tools")
                                Text(connectedDuration(session.connectedSecs))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(.vertical, 5)
                    }
                }
                Section("Remote access") {
                    if model.snapshot.downstreamClients.isEmpty {
                        EmptySectionRow(title: "No remote clients authorized", systemImage: "network")
                    }
                    ForEach(model.snapshot.downstreamClients) { client in
                        HStack(spacing: 12) {
                            Image(systemName: "network")
                                .foregroundStyle(.secondary)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(client.clientName).fontWeight(.medium)
                                Text(client.source.replacingOccurrences(of: "_", with: " ").capitalized)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(client.clientId)
                                .font(.caption.monospaced())
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                                .frame(maxWidth: 180, alignment: .trailing)
                        }
                        .padding(.vertical, 5)
                    }
                }
            }
            .listStyle(.inset)
        }
        .navigationTitle("Clients")
    }

    private var clientSummary: String {
        model.snapshot.liveSessions.isEmpty
            ? "No apps are using Plug right now"
            : "See who is connected and what each client can use"
    }

    private func visibleCount(_ sessionID: String) -> Int {
        model.snapshot.clientVisibility.first { $0.sessionId == sessionID }?.visibleToolCount ?? 0
    }

    private func displayName(_ value: String) -> String {
        value.replacingOccurrences(of: "_", with: " ").capitalized
    }

    private func clientSymbol(_ value: String) -> String {
        value.localizedCaseInsensitiveContains("cursor") ? "cursorarrow.rays" : "terminal"
    }

    private func connectedDuration(_ seconds: UInt64) -> String {
        if seconds < 60 { return "Just connected" }
        if seconds < 3_600 { return "\(seconds / 60)m connected" }
        return "\(seconds / 3_600)h connected"
    }
}
