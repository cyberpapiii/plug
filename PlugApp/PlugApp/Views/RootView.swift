import SwiftUI

/// What the window is for. Signing in to a server used to be its own section,
/// and so did tools; both now live on the server they belong to, because a
/// server's account and its tools are found and fixed where the server is.
enum AppSection: String, CaseIterable, Identifiable, Sendable {
    case servers = "Servers"
    case clients = "Clients"
    case events = "Events"
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

    var body: some View {
        VStack(spacing: 0) {
            if model.verdict.tone != .good {
                VerdictView(verdict: model.verdict, style: .compact, run: run)
                    .padding(.horizontal, Metric.roomy)
                    .padding(.vertical, Metric.snug)
                    .background(.bar)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
            }

            switch router.section {
            case .servers:
                ServersView(model: model, router: router, search: $search, run: run)
            case .clients:
                ClientsView(model: model, search: $search, run: run)
            case .events:
                EventsView(model: model, router: router, search: $search, run: run)
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

        }
        // The system search field: it survives a narrow toolbar, carries the
        // ⌘F shortcut, and clears itself the way every other Mac app does.
        .searchable(
            text: $search,
            placement: .toolbar,
            prompt: searchPrompt
        )
        .navigationTitle("Plug")
        .onChange(of: router.section) {
            search = ""
        }
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
        .overlay(alignment: .bottom) {
            // Any verdict: a press that failed while Plug is unwell still
            // deserves its own sentence, and the verdict never says it.
            if let error = model.actionError {
                ErrorToast(message: error.message) { run(.dismissActionError) }
                    .id(error.id)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
    }

    /// Tools are searched from Servers, so its field says so.
    private var searchPrompt: String {
        switch router.section {
        // The servers prompt names both things it finds and still fits the
        // toolbar field at the minimum window width.
        case .servers: "Servers and tools"
        case .clients: "Search clients"
        case .events: "Search events"
        case .activity: "Search activity"
        }
    }
}
