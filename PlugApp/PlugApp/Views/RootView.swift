import AppKit
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

    var icon: PlugIcon.Kind {
        switch self {
        case .servers: .servers
        case .clients: .clients
        case .events: .events
        case .activity: .activity
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

    @AppStorage("firstMoments") private var storedFirsts: String?
    @State private var firstMoment: FirstRunGuide.Step?
    /// Goes up each time something is added, so the character cheers.
    @State private var cheers = 0

    @State private var columns = NavigationSplitViewVisibility.all
    /// Whether the window is on screen, and whether the model was told so.
    @State private var windowSeen = false
    @State private var isWatching = false
    /// Follows the sidebar, but late on the way back: a second line under
    /// the title while the sidebar is still sliding in does not fit, and the
    /// bar folds its buttons into a "more" button until it does.
    @State private var showsSubtitle = true
    @State private var subtitle: String?

    var body: some View {
        NavigationSplitView(columnVisibility: $columns) {
            List(selection: sidebarSelection) {
                ForEach(AppSection.allCases) { section in
                    Label(section.rawValue, icon: section.icon)
                        .badge(badge(for: section))
                        .tag(section)
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) { sidebarTop }
            .navigationSplitViewColumnWidth(min: 170, ideal: 190, max: 240)
            // The bar's own button lives over the sidebar. While the sidebar
            // slides back in there is no room for it there, so it vanished
            // behind a "more" button and then jumped back. Plug's own sits
            // over the list and moves with it.
            .toolbar(removing: .sidebarToggle)
        } content: {
            section
                .environment(\.splitPane, .list)
                // One width whether the sidebar shows or not: a column that
                // resizes while the sidebar slides makes the slide stutter.
                // Without the sidebar the window's buttons sit over this
                // column, so the title drops its second line to fit.
                .environment(\.showsPageSubtitle, showsSubtitle)
                .task(id: columns == .all) {
                    // From macOS 15 the title keeps this time itself. Set
                    // here, it changed the whole window while the sidebar
                    // was still sliding, and the slide jumped to its end.
                    if #available(macOS 15.0, *) { return }
                    guard columns == .all else {
                        showsSubtitle = false
                        return
                    }
                    try? await Task.sleep(for: .milliseconds(450))
                    if !Task.isCancelled { showsSubtitle = true }
                }
                .navigationSplitViewColumnWidth(min: 240, ideal: Metric.listWidth, max: 420)
                .navigationTitle(router.section.rawValue)
                .toolbar {
                    ToolbarItem(placement: .navigation) {
                        Button {
                            // The system's own slide. Changing the columns from
                            // here in an animation ran half of it and then
                            // jumped to the end.
                            if !SidebarToggle.perform() {
                                withAnimation { columns = columns == .all ? .doubleColumn : .all }
                            }
                        } label: {
                            Label(columns == .all ? "Hide Sidebar" : "Show Sidebar", systemImage: "sidebar.leading")
                        }
                        .help(columns == .all ? "Hide Sidebar" : "Show Sidebar")
                    }
                    titleItem
                }
                // The bar's own title keeps room for a second line it is not
                // showing. With the sidebar hidden that pushed the divider
                // between the columns out into the bar, away from the columns.
                .withoutBarTitle()
                .onPreferenceChange(PageSubtitleKey.self) { subtitle = $0 }
        } detail: {
            section
                .environment(\.splitPane, .detail)
                .toolbar {
                    trailingSpace
                    searchItem
                }
            .topBanner(
                isShown: showsBanner,
                transition: reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity)
            ) {
                VerdictView(verdict: model.verdict, run: run)
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
                } else if let firstMoment, router.sheet == nil {
                    FirstMomentToast(step: firstMoment) { self.firstMoment = nil }
                        .id(firstMoment)
                        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
            }
            .animation(reduceMotion ? nil : .snappy(duration: 0.3), value: firstMoment)
            .animation(reduceMotion ? nil : .snappy(duration: 0.3), value: router.sheet)
            .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: showsBanner)
            .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: model.actionError?.id)
        }
        .plainWindowBar()
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
            case .addClient:
                AddClientView(model: model)
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
        .onChange(of: firstsToLookAt, initial: true) { lookForFirsts() }
        .onChange(of: addedSoFar) { earlier, now in
            // The first look is not an addition.
            if let earlier, let now, now.grew(from: earlier) { cheers += 1 }
        }
        .task { await model.loadConnectableApps() }
        .background(WindowSeenReader(seen: $windowSeen))
        .environment(\.windowSeen, windowSeen)
        .onChange(of: windowSeen) { watch(windowSeen) }
        .onDisappear { watch(false) }
    }

    /// Plug checks the daemon briskly only while this window can be read.
    /// Covered, minimized or on another desktop, it waits like a closed one.
    private func watch(_ now: Bool) {
        guard now != isWatching else { return }
        isWatching = now
        model.setWatching(now)
    }

    private var pageTitle: some View {
        PageTitle(title: router.section.rawValue, subtitle: subtitle, sidebarShown: columns == .all)
    }

    /// The page's title, beside the sidebar button, with no glass behind it.
    @ToolbarContentBuilder private var titleItem: some ToolbarContent {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            ToolbarItem(placement: .navigation) { pageTitle }
                .sharedBackgroundVisibility(.hidden)
        } else if #available(macOS 15.0, *) {
            ToolbarItem(placement: .navigation) { pageTitle }
        }
#else
        if #available(macOS 15.0, *) {
            ToolbarItem(placement: .navigation) { pageTitle }
        }
#endif
    }

    /// Pushes search to the far end of the bar. A plain spacer in an item of
    /// its own is drawn as an empty piece of glass beside it.
    @ToolbarContentBuilder private var trailingSpace: some ToolbarContent {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            ToolbarSpacer(.flexible)
        } else {
            ToolbarItem { Spacer() }
        }
#else
        ToolbarItem { Spacer() }
#endif
    }

    /// The search control draws its own glass, so the bar's is turned off
    /// behind it.
    @ToolbarContentBuilder private var searchItem: some ToolbarContent {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            ToolbarItem(placement: .primaryAction) {
                SearchControl(text: $search, prompt: searchPrompt)
            }
            .sharedBackgroundVisibility(.hidden)
        } else {
            ToolbarItem(placement: .primaryAction) {
                SearchControl(text: $search, prompt: searchPrompt)
            }
        }
#else
        ToolbarItem(placement: .primaryAction) {
            SearchControl(text: $search, prompt: searchPrompt)
        }
#endif
    }

    /// Plug itself at the top of the sidebar: the character, the switch, and
    /// the one line the menu bar panel says.
    private var sidebarTop: some View {
        let verdict = model.verdict
        return VStack(alignment: .leading, spacing: Metric.tight) {
            HStack(spacing: Metric.tight) {
                PlugCharacter(
                    mood: verdict.mood,
                    cheers: cheers,
                    mark: verdict.tone == .blocked ? StatusColor.stopped : StatusColor.needsYou
                )
                .foregroundStyle(.tint)
                .frame(width: 30, height: 30)
                Text("Plug")
                    .font(PanelType.title)
                Spacer(minLength: Metric.tight)
                ServicePowerToggle(model: model, run: run)
                    .labelsHidden()
                    .controlSize(.small)
            }
            Text(verdict.title)
                .font(PanelType.small)
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, Metric.panelInset)
        .padding(.top, Metric.rowGap)
        .padding(.bottom, Metric.snug)
        .accessibilityElement(children: .contain)
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

    /// Where the setup is, once both halves of it have been read. Before
    /// that an empty list does not mean nothing is there.
    private var firstsToLookAt: FirstRunGuide? {
        model.hasLoadedSnapshot && model.hasLoadedConnectableApps ? model.firstRunGuide : nil
    }

    /// What has been added, once both halves of the setup have been read.
    private var addedSoFar: SetupCount? {
        model.hasLoadedSnapshot && model.hasLoadedConnectableApps ? model.setupCount : nil
    }

    private func lookForFirsts() {
        guard let guide = firstsToLookAt else { return }
        var firsts = FirstMoments(stored: storedFirsts)
        if let step = firsts.observe(guide) { firstMoment = step }
        if firsts.stored != storedFirsts { storedFirsts = firsts.stored }
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

/// Search in the window's bar: a magnifying glass until it is pressed, then
/// a field, the way Finder shows it. It is one capsule that grows, inside a
/// slot of fixed width, so the bar itself never has to lay out again. It
/// folds away once it is empty and the click or the cursor goes elsewhere.
private struct SearchControl: View {
    @Binding var text: String
    let prompt: String
    @State private var open = false
    @State private var clicks = ClickAway()
    @FocusState private var focused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static let fieldWidth: CGFloat = 170
    private static let height: CGFloat = 36

    var body: some View {
        HStack(spacing: Metric.tight) {
            Button(action: show) {
                PlugIcon(.search)
                    .frame(width: open ? 18 : Self.height, height: Self.height)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .keyboardShortcut("f")
            .help("Search")
            .accessibilityLabel("Search")
            if open {
                TextField(prompt, text: $text)
                    .textFieldStyle(.plain)
                    .focused($focused)
                    .frame(width: Self.fieldWidth)
                    .onExitCommand(perform: close)
                    .transition(.opacity)
                Button(action: close) {
                    PlugIcon(.dismiss, size: 14)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .opacity(text.isEmpty ? 0 : 1)
                .help("Clear")
                .accessibilityLabel("Clear")
                .accessibilityHidden(text.isEmpty)
            }
        }
        .padding(.horizontal, open ? Metric.snug : 0)
        .frame(height: Self.height)
        .barCapsule()
        .background(ClickAwayAnchor(clicks: clicks))
        .frame(width: Self.fieldWidth + 80, alignment: .trailing)
        .onChange(of: open) {
            clicks.onOutside = open ? { leave() } : nil
        }
        .onChange(of: focused) { if !focused, text.isEmpty { fold() } }
        // A section change empties the search.
        .onChange(of: text) { if text.isEmpty, !focused { fold() } }
        .onDisappear { clicks.onOutside = nil }
    }

    private var motion: Animation? { reduceMotion ? nil : .snappy(duration: 0.28, extraBounce: 0.05) }

    private func show() {
        withAnimation(motion) { open = true }
        focused = true
    }

    private func fold() {
        withAnimation(motion) { open = false }
    }

    /// A click elsewhere: the cursor leaves, and an empty field folds.
    private func leave() {
        focused = false
        if text.isEmpty { fold() }
    }

    private func close() {
        text = ""
        focused = false
        fold()
    }
}

/// Watches for a click outside one view while it is asked to. A text field
/// on the Mac keeps the cursor when the click lands on something that takes
/// no keyboard, so nothing else says the person has moved on.
@MainActor
private final class ClickAway {
    weak var view: NSView?
    private var monitor: Any?

    var onOutside: (() -> Void)? {
        didSet {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
            guard onOutside != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                MainActor.assumeIsolated {
                    guard let self, let view = self.view, event.window == view.window else { return }
                    if !view.bounds.contains(view.convert(event.locationInWindow, from: nil)) {
                        self.onOutside?()
                    }
                }
                return event
            }
        }
    }
}

private struct ClickAwayAnchor: NSViewRepresentable {
    let clicks: ClickAway

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        clicks.view = view
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        clicks.view = view
    }
}


/// Asks the window's split view to hide or show its sidebar, the way the
/// system's own sidebar button does.
@MainActor
private enum SidebarToggle {
    static func perform() -> Bool {
        guard let content = (NSApp.keyWindow ?? NSApp.mainWindow)?.contentView else { return false }
        var queue: [NSView] = [content]
        while !queue.isEmpty {
            let view = queue.removeFirst()
            if let split = view as? NSSplitView, let controller = split.delegate as? NSSplitViewController {
                controller.toggleSidebar(nil)
                return true
            }
            queue.append(contentsOf: view.subviews)
        }
        return false
    }
}

/// The page's title in the window's bar, with the count under it while the
/// sidebar shows. The count follows the sidebar late on the way back: with
/// the sidebar hidden the window's buttons sit over this column, and the
/// second line does not fit until the slide is over.
private struct PageTitle: View {
    let title: String
    let subtitle: String?
    let sidebarShown: Bool
    @State private var showsSubtitle = true

    var body: some View {
        // The bar gives the title the room of its first line and no more, so
        // that room never changes and the bar never moves the title. The
        // second line is drawn over the empty bar beside it: it fades in and
        // the title glides up to make room. Swapped in one step, the title
        // jumped while the sidebar was still moving.
        Text(title)
            .font(.headline)
            .lineLimit(1)
            .fixedSize()
            .hidden()
            .overlay(alignment: .leading) {
                lines.animation(.smooth(duration: 0.25), value: showsSubtitle)
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            .task(id: sidebarShown) {
                guard sidebarShown else {
                    showsSubtitle = false
                    return
                }
                try? await Task.sleep(for: .milliseconds(450))
                if !Task.isCancelled { showsSubtitle = true }
            }
    }

    private var lines: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.headline)
            if showsSubtitle, let subtitle, !subtitle.isEmpty {
                Text(subtitle)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .transition(.opacity)
            }
        }
        .lineLimit(1)
        .fixedSize()
    }
}

private extension View {
    /// The bar's own glass, as a capsule.
    @ViewBuilder
    func barCapsule() -> some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            glassEffect(.regular.interactive(), in: .capsule)
        } else {
            background(.quaternary, in: Capsule())
        }
#else
        background(.quaternary, in: Capsule())
#endif
    }


    /// Takes the bar's own title away where Plug draws it. Before macOS 15
    /// the bar keeps its own.
    @ViewBuilder func withoutBarTitle() -> some View {
        if #available(macOS 15.0, *) {
            toolbar(removing: .title)
        } else {
            self
        }
    }

    /// No bar background behind the window's toolbar. With one, macOS fades
    /// a line in under each column's bar whenever the pointer is over it.
    @ViewBuilder
    func plainWindowBar() -> some View {
        if #available(macOS 15.0, *) {
            toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        } else {
            self
        }
    }

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
