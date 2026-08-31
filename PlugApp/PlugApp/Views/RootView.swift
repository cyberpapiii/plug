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

/// Four persistent destinations belong in the system sidebar. This keeps
/// search, filters, and actions out of a crowded title-bar control.
struct RootView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void

    var body: some View {
        NavigationSplitView {
            List(AppSection.allCases, selection: $router.section) { section in
                Label(section.rawValue, systemImage: section.symbol)
                    .tag(section)
            }
            .navigationTitle("Plug")
            .navigationSplitViewColumnWidth(min: 168, ideal: 188, max: 220)
        } detail: {
            content
                .navigationTitle(router.section.rawValue)
                .safeAreaInset(edge: .top, spacing: 0) {
                    if model.verdict.tone != .good {
                        VerdictView(verdict: model.verdict, compact: true, run: run)
                            .padding(.horizontal, Metric.regular)
                            .padding(.vertical, Metric.snug)
                            .nativeGlassSurface()
                            .padding(.horizontal, Metric.regular)
                            .padding(.bottom, Metric.tight)
                            .transition(.move(edge: .top).combined(with: .opacity))
                    }
                }
        }
        .navigationSplitViewStyle(.balanced)
        .animation(.snappy(duration: 0.2), value: model.verdict)
        .toolbar {
            ToolbarItem {
                SettingsLink {
                    Image(systemName: "gearshape")
                }
                .help("Settings")
                .accessibilityLabel("Settings")
            }
        }
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
        .overlay(alignment: .bottom) {
            if let error = model.lastError, model.verdict.tone == .good {
                ErrorToast(message: error)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
    }

    @ViewBuilder private var content: some View {
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
}
