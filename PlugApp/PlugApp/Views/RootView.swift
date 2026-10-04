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

    /// Plug itself, where a picture puts it between servers and clients.
    static let plugSymbol = "bolt"

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
        } content: {
            section
                .environment(\.splitPane, .list)
                .navigationSplitViewColumnWidth(min: 240, ideal: Metric.listWidth, max: 420)
                .navigationTitle(router.section.rawValue)
        } detail: {
            section
                .environment(\.splitPane, .detail)
            .topBanner(
                isShown: showsBanner,
                transition: reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity)
            ) {
                VerdictView(verdict: model.verdict, style: .compact, run: run)
                    .padding(.horizontal, Metric.roomy)
                    .padding(.vertical, Metric.snug)
            }
            .overlay(alignment: .bottom) {
                // Any verdict: a press that failed while Plug is unwell still
                // deserves its own sentence, and the verdict never says it.
                if let error = model.actionError {
                    ErrorToast(error: error) { run(.dismissActionError) }
                        .id(error.id)
                        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
            }
            .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: showsBanner)
            .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: model.actionError?.id)
        }
        .searchable(text: $search, placement: .toolbar, prompt: searchPrompt)
        .onChange(of: router.section) {
            search = ""
        }
        // The window shows one sheet at a time. Setting another replaces it.
        .sheet(item: $router.sheet) { sheet in
            switch sheet {
            case .guide:
                GuideView(model: model) { intent in
                    guideSeen = true
                    if let intent { run(intent) }
                    // An intent that opens a sheet has already replaced the guide.
                    if router.sheet == .guide { router.sheet = nil }
                }
            case .addServer:
                AddServerView(model: model, router: router)
            case .importServers:
                ImportServersView(model: model, router: router)
            case .addWatch:
                AddWatchView(model: model)
            case let .editServer(name):
                EditServerView(model: model, name: name)
            case let .addAccount(server):
                AddAccountView(model: model, router: router, server: server)
            }
        }
        .onChange(of: router.sheet) { old, _ in
            // However the guide was closed, it has been seen.
            if old == .guide { guideSeen = true }
        }
        .onChange(of: guideOpensByItself, initial: true) {
            if guideOpensByItself, router.sheet == nil { router.sheet = .guide }
        }
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
    }

    /// The section in view. The window draws it in two columns, the rows in
    /// the middle one and the selected row in the last, and the section's
    /// views pick their half from the environment.
    @ViewBuilder private var section: some View {
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

    /// The banner keeps quiet while the page itself is saying the same thing:
    /// a page that is loading or unavailable shows the verdict, and an empty
    /// page already offers Add Server.
    private var showsBanner: Bool {
        if model.isLoadingInitialData || model.initialDataUnavailable { return false }
        let verdict = model.verdict
        if verdict.tone == .good || verdict.tone == .busy { return false }
        return verdict.primary?.intent != .addServer
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
        case .servers: "Search servers and tools"
        case .clients: "Search clients"
        case .events: "Search events"
        case .activity: "Search activity"
        }
    }
}

private extension View {
    /// The verdict banner above a section. On macOS 26 it is a safe area bar,
    /// so the toolbar's scroll edge effect carries on under it; before that it
    /// is an inset on the bar material with its own divider.
    @ViewBuilder
    func topBanner<Banner: View>(
        isShown: Bool,
        transition: AnyTransition,
        @ViewBuilder _ banner: () -> Banner
    ) -> some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            safeAreaBar(edge: .top, spacing: 0) {
                if isShown { banner().transition(transition) }
            }
        } else {
            legacyTopBanner(isShown: isShown, transition: transition, banner)
        }
#else
        legacyTopBanner(isShown: isShown, transition: transition, banner)
#endif
    }

    func legacyTopBanner<Banner: View>(
        isShown: Bool,
        transition: AnyTransition,
        @ViewBuilder _ banner: () -> Banner
    ) -> some View {
        safeAreaInset(edge: .top, spacing: 0) {
            if isShown {
                VStack(spacing: 0) {
                    banner()
                    Divider()
                }
                .background(.bar)
                .transition(transition)
            }
        }
    }
}
