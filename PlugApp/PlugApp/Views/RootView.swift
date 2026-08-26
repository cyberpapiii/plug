import SwiftUI

/// The three things worth a window. Everything that used to be a fourth
/// section — signing in to a server — now lives on the server itself, because
/// a problem should be fixable where it is seen.
enum AppSection: String, CaseIterable, Identifiable, Sendable {
    case servers = "Servers"
    case connections = "Connections"
    case activity = "Activity"

    var id: Self { self }

    var symbol: String {
        switch self {
        case .servers: "shippingbox"
        case .connections: "app.connected.to.app.below.fill"
        case .activity: "clock.arrow.circlepath"
        }
    }
}

/// The window. No sidebar: three peers do not earn a permanent column, and the
/// space is better spent on the content itself.
struct RootView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void

    var body: some View {
        VStack(spacing: 0) {
            if model.verdict.tone != .good {
                VerdictView(verdict: model.verdict, compact: true, run: run)
                    .padding(.horizontal, Metric.roomy)
                    .padding(.vertical, Metric.snug)
                    .background(.bar)
                    .overlay(alignment: .bottom) { Divider() }
                    .transition(.move(edge: .top).combined(with: .opacity))
            }

            switch router.section {
            case .servers:
                ServersView(model: model, router: router, run: run)
            case .connections:
                ConnectionsView(model: model, run: run)
            case .activity:
                ActivityView(model: model)
            }
        }
        .animation(.snappy(duration: 0.2), value: model.verdict)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Picker("Section", selection: $router.section) {
                    ForEach(AppSection.allCases) { section in
                        Text(section.rawValue).tag(section)
                    }
                }
                .pickerStyle(.segmented)
                .frame(minWidth: 260)
            }
        }
        .navigationTitle("Plug")
        .navigationSubtitle(model.situation.version.isEmpty ? "" : "Version \(model.situation.version)")
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
        .overlay(alignment: .bottom) {
            if let error = model.lastError, model.verdict.tone == .good {
                ErrorToast(message: error)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
    }
}
