import PlugIPC
import SwiftUI

/// One server, in full. This absorbed the old Auth and Tools sections: an
/// account that needs signing in and the tools a server offers both belong to
/// that server, so they are shown, fixed, and switched here.
struct ServerDetailView: View {
    let model: AppModel
    let server: ServerFacts
    /// The window's search. It narrows the tool list below.
    let query: String
    let router: Router
    let run: (PlugIntent) -> Void
    /// Asks the list to confirm removing this server; the list owns the question.
    let onRemove: () -> Void
    @State private var confirmSignOut = false
    /// Where this server's own settings are. Nil until Plug has read them.
    @State private var settingsPlace: ServerSettingsPlace?

    var body: some View {
        DetailForm {
            Section {
                header
                if server.health == .signInNeeded {
                    signInNote
                } else if let fix = server.fix {
                    ProblemNote(
                        title: server.problem,
                        reason: server.error,
                        actionTitle: fix.title,
                        action: { run(fix.intent) },
                        failed: server.health == .down || server.health == .unknown
                    )
                }
            }
            details
            ServerRecentCalls(model: model, server: server.name)
            ServerTools(
                model: model,
                server: server.name,
                isWorking: server.health == .working,
                query: query,
                router: router,
                run: run
            )
            .equatable()
        }
        .task(id: server.name) { await findSettingsPlace() }
        .confirmationDialog(
            "Sign out of \(server.name)?",
            isPresented: $confirmSignOut,
            titleVisibility: .visible
        ) {
            Button("Sign Out", role: .destructive) { run(.signOut(server: server.name)) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Plug forgets the stored account. The server stops working until you sign in again.")
        }
    }

    // MARK: - Header

    private var header: some View {
        DetailHeader(title: server.name, subtitle: statusLine) {
            ServerGlyph(
                name: server.name,
                size: Metric.glyphSlot,
                status: server.enabled ? server.health.color : nil
            )
            .opacity(server.enabled ? 1 : 0.5)
        } controls: {
            Button("Edit…") { run(.editServer(server.name)) }
            Menu {
                if server.enabled {
                    Button("Restart") { run(.restartServer(server.name)) }
                    Divider()
                }
                Button("Add Another Account…") { run(.addAccount(server: server.name)) }
                if server.usesOAuth, server.health != .signInNeeded {
                    Button("Sign Out…") { confirmSignOut = true }
                }
                Divider()
                Button("Remove Server…", role: .destructive, action: onRemove)
            } label: {
                Label("More", icon: .more)
            }
            .labelStyle(.iconOnly)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("More")
            Toggle(
                "On",
                isOn: Binding(
                    get: { server.enabled },
                    set: { run(.setServerEnabled(server.name, $0)) }
                )
            )
            .labelsHidden()
            .toggleStyle(.switch)
            .help(server.enabled ? "Turn off \(server.name)" : "Turn on \(server.name)")
            .accessibilityLabel(server.name)
        }
        .disabled(!model.canMutate)
    }

    private var statusLine: String {
        switch server.health {
        case .working: "\(server.health.label) · \(server.toolCountText)"
        default: server.health.label
        }
    }

    // MARK: - Problem

    @ViewBuilder private var signInNote: some View {
        if server.isSigningIn {
            VStack(alignment: .leading, spacing: Metric.snug) {
                ProblemNote(title: "Finish signing in with your browser.")
                HStack(spacing: Metric.tight) {
                    Button("Try Again") { run(.signIn(server: server.name)) }
                    Button("Cancel") { run(.cancelSignIn(server: server.name)) }
                }
            }
        } else {
            ProblemNote(
                title: "Sign in to your account to use this server.",
                actionTitle: "Sign In",
                action: { run(.signIn(server: server.name)) }
            )
        }
    }

    // MARK: - Details

    private var details: some View {
        // Worked out once: it walks every tool the server has.
        let groups = toolGroups
        return Section(heading: "Details") {
            LabeledContent("Runs", value: server.transportLabel)
            if let settingsPlace {
                LabeledContent("Settings") {
                    HStack(spacing: Metric.snug) {
                        Text(settingsPlace.label())
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Button(settingsPlace.actionTitle) {
                            if settingsPlace == .plug { run(.editServer(server.name)) } else { settingsPlace.open() }
                        }
                    }
                }
            }
            if server.usesOAuth, server.health != .signInNeeded {
                LabeledContent("Account", value: accountLabel)
            }
            if groups.count > 1 {
                LabeledContent("Clients See") {
                    Text(groups.joined(separator: ", "))
                        .multilineTextAlignment(.trailing)
                }
            }
            ForEach(server.authWarnings, id: \.self) { warning in
                Label {
                    Text(warning)
                } icon: {
                    PlugIcon(.needsYou)
                        .foregroundStyle(StatusColor.needsYou)
                }
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func findSettingsPlace() async {
        settingsPlace = nil
        guard let config = try? await model.serverConfig(name: server.name) else { return }
        let described = (try? await model.serverDescriptions()) ?? []
        let website = described.first { $0.serverId == server.name }?.upstream?.websiteUrl
        guard !Task.isCancelled else { return }
        settingsPlace = ServerSettingsPlace.find(config: config, website: website)
    }

    private var accountLabel: String {
        guard let seconds = server.tokenExpiresInSecs else { return "Signed in" }
        if seconds >= 86_400 {
            let days = seconds / 86_400
            return "Signed in · renews in \(days) \(days == 1 ? "day" : "days")"
        }
        if seconds >= 3_600 { return "Signed in · renews in \(seconds / 3_600) hr" }
        return "Signed in · renews soon"
    }

    /// The groups this server's tools are split into, when there are several.
    private var toolGroups: [String] {
        ToolCatalog.groups(ofNames: model.toolCatalog.tools(for: server.name).map(\.name))
    }
}

/// A server's latest calls. A view of its own, so a call arriving redraws
/// these few rows and not the page around them.
private struct ServerRecentCalls: View {
    let model: AppModel
    let server: String

    var body: some View {
        let calls = model.recentActivity(for: server, limit: 6)
        if !calls.isEmpty {
            Section(heading: "Recent Activity") {
                ForEach(calls) { event in
                    let call = model.call(event)
                    HStack(spacing: Metric.tight) {
                        ToolName(call.tool)
                        Spacer(minLength: Metric.tight)
                        if !call.succeeded {
                            Label {
                                Text(call.result).foregroundStyle(.secondary)
                            } icon: {
                                PlugIcon(call.failed ? .stopped : .skipped)
                                    .foregroundStyle(call.failed ? StatusColor.stopped : StatusColor.quiet)
                            }
                            .font(.callout)
                            .lineLimit(1)
                        }
                        Text(call.duration)
                            .font(PanelType.small.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                    .help(call.reason ?? call.result)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("\(call.tool), \(call.result), \(call.spokenDuration)")
                }
            }
        }
    }
}

/// A server's tools. A view of its own that is equal while what it shows is
/// the same, so the rest of the page changing does not rebuild every row.
private struct ServerTools: View, Equatable {
    let model: AppModel
    let server: String
    let isWorking: Bool
    /// The window's search. It narrows the list.
    let query: String
    let router: Router
    let run: (PlugIntent) -> Void
    @State private var showsOffOnly = false
    /// The list the rows were last built for, and how many are built so far.
    @State private var builtFor: Build?
    @State private var builtCount = ServerTools.firstRows

    /// A long list is built a screenful first and the rest right after, so
    /// the page appears at once instead of after every row is made.
    private static let firstRows = 30
    private static let moreRows = 30

    /// One list to build: a server, and whether its tools have arrived.
    private struct Build: Hashable {
        let server: String
        let hasTools: Bool
    }

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.model === rhs.model && lhs.router === rhs.router
            && lhs.server == rhs.server && lhs.isWorking == rhs.isWorking && lhs.query == rhs.query
    }

    var body: some View {
        let all = model.toolCatalog.tools(for: server)
        let matched = model.toolCatalog.tools(for: server, matching: query)
        let shown = showsOffOnly ? matched.filter { !$0.isOn } : matched
        let canManage = model.canManageTools && model.canMutate
        // Read once here, so opening a tool's details redraws two rows and
        // not every row.
        let selected = router.selectedTool
        let build = Build(server: server, hasTools: !all.isEmpty)
        let limit = builtFor == build ? builtCount : Self.firstRows
        Section {
            if all.isEmpty {
                Text(isWorking ? "This server offers no tools." : "Tools appear once the server is running.")
                    .foregroundStyle(.secondary)
            } else if shown.isEmpty {
                Text(noToolsShown)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(shown.prefix(limit)) { tool in
                    ServerToolRow(
                        tool: tool,
                        canManage: canManage,
                        isBusy: model.busyTools.contains(tool.name),
                        showsDetails: selected == tool.name,
                        model: model,
                        router: router,
                        run: run
                    )
                    .equatable()
                }
            }
        } header: {
            HStack(spacing: Metric.snug) {
                Text("Tools")
                    .font(PanelType.small.weight(.semibold))
                    .foregroundStyle(.secondary)
                if let summary = summary(total: all.count, shown: shown.count, off: all.lazy.filter { !$0.isOn }.count) {
                    Text(summary)
                        .font(PanelType.small.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                if !all.isEmpty {
                    Picker("Show", selection: $showsOffOnly) {
                        Text("All").tag(false)
                        Text("Off").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .controlSize(.small)
                    .fixedSize()
                }
            }
            .task(id: build) { await buildRows(for: build) }
        }
    }

    /// Adds the rows after the first screenful, a batch per frame.
    private func buildRows(for build: Build) async {
        var count = Self.firstRows
        builtCount = count
        builtFor = build
        while count < model.toolCatalog.tools(for: server).count {
            try? await Task.sleep(nanoseconds: 16_000_000)
            if Task.isCancelled { return }
            count += Self.moreRows
            builtCount = count
        }
        builtCount = .max
    }

    /// Nil when every tool is on and shown: the header above already gives
    /// the count.
    private func summary(total: Int, shown: Int, off: Int) -> String? {
        if shown < total, !showsOffOnly { return "\(shown) of \(total)" }
        if off > 0 { return "\(total - off) of \(total) on" }
        return nil
    }

    private var noToolsShown: String {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        if !showsOffOnly { return "No tools match “\(trimmed)”." }
        return trimmed.isEmpty ? "No tools are off." : "No tools that are off match this search."
    }
}

/// One tool on a server's page, with the details it opens. Equal while the
/// tool and what may be done to it are the same.
private struct ServerToolRow: View, Equatable {
    let tool: ToolFacts
    let canManage: Bool
    let isBusy: Bool
    /// Whether this is the tool whose details are open.
    let showsDetails: Bool
    let model: AppModel
    let router: Router
    let run: (PlugIntent) -> Void

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.tool == rhs.tool && lhs.canManage == rhs.canManage && lhs.isBusy == rhs.isBusy
            && lhs.showsDetails == rhs.showsDetails
            && lhs.model === rhs.model && lhs.router === rhs.router
    }

    var body: some View {
        ToolRow(
            tool: tool,
            canManage: canManage,
            isBusy: isBusy,
            onSelect: { router.selectedTool = tool.name },
            run: run
        )
        .accessibilityAction(named: "Show Details") { router.selectedTool = tool.name }
        // Only the open row carries a popover. It sits behind the row, the
        // row's own size, so the arrow points where it always did.
        .background {
            if showsDetails { detailsAnchor }
        }
    }

    private var detailsAnchor: some View {
        Color.clear
            .allowsHitTesting(false)
            .popover(
                isPresented: Binding(
                    get: { showsDetails },
                    set: { shown in if !shown, router.selectedTool == tool.name { router.selectedTool = nil } }
                ),
                arrowEdge: .trailing
            ) {
                ToolDetailView(
                    tool: tool,
                    catalog: model.toolCatalog,
                    canManage: canManage,
                    isBusy: isBusy,
                    run: run
                )
                .frame(width: 320)
            }
    }
}
