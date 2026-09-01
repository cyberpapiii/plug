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
}

/// The window. No sidebar: four peers do not earn a permanent column, and the
/// space is better spent on the content itself.
struct RootView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var search = ""
    @FocusState private var searchFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            if model.verdict.tone != .good {
                VerdictView(verdict: model.verdict, compact: true, run: run)
                    .padding(.horizontal, Metric.roomy)
                    .padding(.vertical, Metric.snug)
                    .background(.bar)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
            }

            switch router.section {
            case .servers:
                ServersView(model: model, router: router, search: $search, run: run)
            case .tools:
                ToolsView(model: model, router: router, query: $search, run: run)
            case .connections:
                ConnectionsView(model: model, search: $search, run: run)
            case .activity:
                ActivityView(model: model, search: $search, run: run)
            }
        }
        .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: model.verdict)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Picker("Section", selection: $router.section) {
                    ForEach(AppSection.allCases) { section in
                        Text(section.rawValue).tag(section)
                    }
                }
                .pickerStyle(.segmented)
                .frame(minWidth: 280, idealWidth: 340, maxWidth: 340)
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

            ToolbarItem {
                TextField("Search", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .frame(minWidth: 120, idealWidth: 160, maxWidth: 160)
                    .focused($searchFocused)
                    .accessibilityLabel("Search \(router.section.rawValue.lowercased())")
            }
        }
        .navigationTitle("Plug")
        .onChange(of: router.section) {
            search = ""
            searchFocused = false
        }
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
        .overlay(alignment: .bottom) {
            if let error = model.lastError, model.verdict.tone == .good {
                ErrorToast(message: error)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
    }

}
