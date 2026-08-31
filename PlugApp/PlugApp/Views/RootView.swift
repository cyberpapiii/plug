import SwiftUI

/// What the window is for. Signing in to a server used to be its own section;
/// it now lives on the server itself, because a problem should be fixable
/// where it is seen. Tools are their own section because they are what a
/// connected app actually gets, and they are switched on and off one by one.
enum AppSection: String, CaseIterable, Identifiable, Sendable {
    case servers = "Servers"
    case tools = "Tools"
    case connections = "Connections"
    case activity = "Activity"

    var id: Self { self }

    var symbol: String {
        switch self {
        case .servers: "shippingbox"
        case .tools: "wrench.and.screwdriver"
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
            case .tools:
                ToolsView(model: model, router: router, run: run)
            case .connections:
                ConnectionsView(model: model, run: run)
            case .activity:
                ActivityView(model: model)
            }
        }
        .animation(.snappy(duration: 0.2), value: model.verdict)
        .toolbar {
            ToolbarItem(placement: .principal) {
                // Icon and word together: the picture is what people aim at
                // after the first visit, the word is what makes the first visit
                // work.
                Picker("Section", selection: $router.section) {
                    ForEach(AppSection.allCases) { section in
                        Label(section.rawValue, systemImage: section.symbol).tag(section)
                    }
                }
                .pickerStyle(.segmented)
                .frame(minWidth: 330)
            }

            // Plug has no menu bar of its own — it is an accessory app — so the
            // window carries the way into Settings itself.
            ToolbarItem {
                SettingsLink {
                    Image(systemName: "gearshape")
                }
                .help("Settings")
                .accessibilityLabel("Settings")
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
