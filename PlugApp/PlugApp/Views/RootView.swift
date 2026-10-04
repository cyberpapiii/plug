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

    var symbol: String {
        switch self {
        case .servers: "shippingbox"
        case .clients: "macwindow.on.rectangle"
        case .events: "bell"
        case .activity: "clock"
        }
    }
}

/// The window, laid out the way a Mac window is: the sections down a sidebar,
/// and each section a list beside the row that is selected. Settings is its
/// own window, opened the way every Mac app opens it.
struct RootView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var search = ""
    @AppStorage(RootView.guideSeenKey) private var guideSeen = false

    static let guideSeenKey = "guideSeen"

    var body: some View {
        NavigationSplitView {
            List(selection: sidebarSelection) {
                ForEach(AppSection.allCases) { section in
                    Label(section.rawValue, systemImage: section.symbol)
                        .badge(badge(for: section))
                        .tag(section)
                }
            }
            .navigationSplitViewColumnWidth(min: 150, ideal: 170, max: 220)
        } detail: {
            Group {
                switch router.section {
                case .servers:
                    ServersView(model: model, router: router, search: $search, run: run)
                case .clients:
                    ClientsView(model: model, router: router, search: $search, run: run)
                case .events:
                    EventsView(model: model, router: router, search: $search, run: run)
                case .activity:
                    ActivityView(model: model, router: router, search: $search, run: run)
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                if model.verdict.tone != .good {
                    VStack(spacing: 0) {
                        VerdictView(verdict: model.verdict, style: .compact, run: run)
                            .padding(.horizontal, Metric.roomy)
                            .padding(.vertical, Metric.snug)
                        Divider()
                    }
                    .background(.bar)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
                }
            }
            .navigationTitle(router.section.rawValue)
            .searchable(text: $search, placement: .toolbar, prompt: searchPrompt)
        }
        .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: model.verdict)
        .onChange(of: router.section) {
            search = ""
        }
        .sheet(isPresented: $router.isShowingGuide, onDismiss: { guideSeen = true }) {
            GuideView(model: model) { intent in
                router.isShowingGuide = false
                guard let intent else { return }
                // The window shows one sheet at a time, so the next one waits
                // for this one to finish closing.
                Task {
                    try? await Task.sleep(for: .milliseconds(400))
                    run(intent)
                }
            }
        }
        .sheet(isPresented: $router.isAddingServer) {
            AddServerView(model: model)
        }
        .sheet(isPresented: $router.isImportingServers) {
            ImportServersView(model: model)
        }
        .sheet(item: $router.editingServer) { target in
            EditServerView(model: model, name: target.id)
        }
        .sheet(item: $router.addingAccountTo) { target in
            AddAccountView(model: model, router: router, server: target.id)
        }
        .sheet(isPresented: $router.isAddingWatch) {
            AddWatchView(model: model)
        }
        .onChange(of: guideOpensByItself, initial: true) {
            if guideOpensByItself { router.isShowingGuide = true }
        }
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
        .overlay(alignment: .bottom) {
            // Any verdict: a press that failed while Plug is unwell still
            // deserves its own sentence, and the verdict never says it.
            if let error = model.actionError {
                ErrorToast(error: error) { run(.dismissActionError) }
                    .id(error.id)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
    }

    /// The sidebar always has one section selected.
    private var sidebarSelection: Binding<AppSection?> {
        Binding(get: { router.section }, set: { if let section = $0 { router.section = section } })
    }

    /// The one number worth carrying in the sidebar: servers that need the
    /// owner. Zero shows nothing.
    private func badge(for section: AppSection) -> Int {
        section == .servers ? model.situation.troubledServers.count : 0
    }

    private var guideOpensByItself: Bool {
        FirstRunGuide.opensByItself(
            seen: guideSeen,
            loaded: !model.isLoadingInitialData && !model.initialDataUnavailable,
            serverCount: model.snapshot.configuredServers.count
        )
    }

    /// Tools are searched from Servers, so its field says so.
    private var searchPrompt: String {
        switch router.section {
        case .servers: "Servers and tools"
        case .clients: "Search clients"
        case .events: "Search events"
        case .activity: "Search activity"
        }
    }
}
