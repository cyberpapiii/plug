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
                SidebarSectionRow(
                    section: section,
                    detail: sidebarDetail(for: section)
                )
                .tag(section)
            }
            .listStyle(.sidebar)
            .navigationTitle("Plug")
            .navigationSplitViewColumnWidth(min: 168, ideal: 188, max: 220)
            .safeAreaInset(edge: .bottom) {
                RuntimeFooter(model: model)
            }
        } detail: {
            VStack(spacing: 0) {
                if model.showsReconciliationProgress {
                    ReconciliationProgressNotice()
                } else if model.adoptionIsRequired {
                    ServiceAdoptionNotice(model: model)
                } else if let failure = model.installationFailure {
                    InstallationFailureNotice(model: model, failure: failure)
                } else if let drift = model.installationDrift {
                    InstallationDriftNotice(drift: drift)
                }
                Group {
                    switch selection ?? .servers {
                    case .servers: ServersView(model: model)
                    case .clients: ClientsView(model: model)
                    case .activity: ActivityView(model: model)
                    case .auth: AuthView(model: model)
                    }
                }
            }
            .toolbar {
                Button { Task { await model.refresh() } } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .keyboardShortcut("r", modifiers: .command)
            }
        }
        .overlay(alignment: .bottom) {
            if let error = model.lastError, model.installationFailure == nil {
                ErrorToast(message: error)
            }
        }
    }

    private func sidebarDetail(for section: AppSection) -> String {
        switch section {
        case .servers:
            let enabled = model.visibleServers.filter(\.configured.enabled).count
            return "\(enabled) enabled"
        case .clients:
            return "\(model.snapshot.liveSessions.count) connected"
        case .activity:
            return model.activities.isEmpty ? "No recent calls" : "\(model.activities.count) recent"
        case .auth:
            let attention = model.snapshot.upstreamAuth.filter { !$0.authenticated }.count
            return attention == 0 ? "All connected" : "\(attention) need attention"
        }
    }
}
