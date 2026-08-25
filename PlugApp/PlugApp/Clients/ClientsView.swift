import SwiftUI

struct ClientsView: View {
    let model: AppModel
    var body: some View {
        List {
            Section("Connected now") {
                ForEach(model.snapshot.liveSessions) { session in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(session.clientInfo ?? session.clientType)
                            Text(session.transport).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Text("\(visibleCount(session.id)) tools").foregroundStyle(.secondary)
                    }
                }
                if model.snapshot.liveSessions.isEmpty { Text("No clients connected").foregroundStyle(.secondary) }
            }
            Section("Remote grants") {
                ForEach(model.snapshot.downstreamClients) { client in
                    VStack(alignment: .leading) { Text(client.clientName); Text(client.clientID).font(.caption).foregroundStyle(.secondary) }
                }
                if model.snapshot.downstreamClients.isEmpty { Text("No remote clients authorized").foregroundStyle(.secondary) }
            }
        }.navigationTitle("Clients")
    }
    private func visibleCount(_ sessionID: String) -> Int { model.snapshot.clientVisibility.first { $0.sessionID == sessionID }?.visibleToolCount ?? 0 }
}
