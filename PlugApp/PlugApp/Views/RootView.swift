import SwiftUI

enum AppSection: String, CaseIterable, Identifiable {
    case servers = "Servers", clients = "Clients", activity = "Activity", auth = "Auth"
    var id: Self { self }
    var symbol: String {
        switch self { case .servers: "shippingbox"; case .clients: "person.2"; case .activity: "waveform.path.ecg"; case .auth: "key" }
    }
}

struct RootView: View {
    let model: AppModel
    @State private var selection: AppSection? = .servers

    var body: some View {
        NavigationSplitView {
            List(AppSection.allCases, selection: $selection) { section in
                Label(section.rawValue, systemImage: section.symbol).tag(section)
            }
            .listStyle(.sidebar)
            .navigationTitle("Plug")
        } detail: {
            Group {
                switch selection ?? .servers {
                case .servers: ServersView(model: model)
                case .clients: ClientsView(model: model)
                case .activity: ActivityView(model: model)
                case .auth: AuthView(model: model)
                }
            }
            .toolbar { Button { Task { await model.refresh() } } label: { Label("Refresh", systemImage: "arrow.clockwise") } }
        }
        .overlay(alignment: .bottom) {
            if let error = model.lastError {
                Text(error).font(.callout).padding(10).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10)).padding()
            }
        }
        .safeAreaInset(edge: .top) {
            if model.serviceNeedsAdoption {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Let Plug manage its background service").fontWeight(.semibold)
                        Text("This replaces the older command-line startup entry and keeps one daemon in charge.").font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Use Plug") { Task { await model.adoptDaemon() } }.buttonStyle(.borderedProminent)
                }.padding(12).background(.regularMaterial)
            }
        }
    }
}
