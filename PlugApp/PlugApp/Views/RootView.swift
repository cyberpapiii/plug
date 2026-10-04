import SwiftUI

/// What the window is for. Signing in to a server used to be its own section,
/// and so did tools; both now live on the server they belong to, because a
/// server's account and its tools are found and fixed where the server is.
enum AppSection: String, CaseIterable, Identifiable, Sendable {
    case servers = "Servers"
    case clients = "Clients"
    case events = "Events"
    case activity = "Activity"
    case settings = "Settings"

    var id: Self { self }
}

/// The window, and the only one: Settings is its fifth section, so the switch
/// that turns Plug off and the checkup sit beside the things they affect. No
/// sidebar: five peers do not earn a permanent column.
struct RootView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var search = ""
    @AppStorage(RootView.guideSeenKey) private var guideSeen = false

    static let guideSeenKey = "guideSeen"

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
            case .settings:
                SettingsView(model: model, router: router, run: run)
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
                .frame(minWidth: 340, idealWidth: 420, maxWidth: 420)
            }

            ToolbarItem {
                Button { run(.showGuide) } label: {
                    Image(systemName: "questionmark.circle")
                }
                .help("How Plug works")
                .accessibilityLabel("How Plug works")
            }

        }
        .modifier(SectionSearch(text: $search, prompt: searchPrompt))
        .navigationTitle("Plug")
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
        .onChange(of: guideOpensByItself, initial: true) {
            if guideOpensByItself { router.isShowingGuide = true }
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

    private var guideOpensByItself: Bool {
        FirstRunGuide.opensByItself(
            seen: guideSeen,
            loaded: !model.isLoadingInitialData && !model.initialDataUnavailable,
            serverCount: model.snapshot.configuredServers.count
        )
    }

    /// Tools are searched from Servers, so its field says so.
    private var searchPrompt: String? {
        switch router.section {
        // The servers prompt names both things it finds and still fits the
        // toolbar field at the minimum window width.
        case .servers: "Servers and tools"
        case .clients: "Search clients"
        case .events: "Search events"
        case .activity: "Search activity"
        case .settings: nil
        }
    }
}

/// The system search field: it survives a narrow toolbar, carries the ⌘F
/// shortcut, and clears itself the way every other Mac app does. A section
/// with nothing to search shows no field.
private struct SectionSearch: ViewModifier {
    @Binding var text: String
    let prompt: String?

    func body(content: Content) -> some View {
        if let prompt {
            content.searchable(text: $text, placement: .toolbar, prompt: prompt)
        } else {
            content
        }
    }
}
