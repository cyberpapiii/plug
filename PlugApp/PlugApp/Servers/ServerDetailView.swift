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
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    /// Asks the list to confirm removing this server; the list owns the question.
    let onRemove: () -> Void
    @State private var confirmSignOut = false
    @State private var showsOffOnly = false
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
            if !recentCalls.isEmpty { recent }
            tools
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
        Section(heading: "Details") {
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
            if toolGroups.count > 1 {
                LabeledContent("Clients See") {
                    Text(toolGroups.joined(separator: ", "))
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

    // MARK: - Recent

    private var recentCalls: [ActivityEvent] {
        model.recentActivity(for: server.name, limit: 6)
    }

    private var recent: some View {
        Section(heading: "Recent Activity") {
            ForEach(recentCalls) { event in
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

    // MARK: - Tools

    private var allTools: [ToolFacts] { model.toolCatalog.tools(for: server.name) }
    /// The groups this server's tools are split into, when there are several.
    private var toolGroups: [String] { ToolCatalog.groups(ofNames: allTools.map(\.name)) }
    private var offCount: Int { allTools.filter { !$0.isOn }.count }

    private var shownTools: [ToolFacts] {
        let matched = model.toolCatalog.tools(for: server.name, matching: query)
        return showsOffOnly ? matched.filter { !$0.isOn } : matched
    }

    /// Nil when every tool is on and shown: the header above already gives
    /// the count.
    private var toolsSummary: String? {
        let total = allTools.count
        let shown = shownTools.count
        if shown < total, !showsOffOnly { return "\(shown) of \(total)" }
        if offCount > 0 { return "\(total - offCount) of \(total) on" }
        return nil
    }

    private var trimmedQuery: String { query.trimmingCharacters(in: .whitespaces) }

    private var noToolsShown: String {
        if !showsOffOnly { return "No tools match “\(trimmedQuery)”." }
        return trimmedQuery.isEmpty ? "No tools are off." : "No tools that are off match this search."
    }

    private var tools: some View {
        Section {
            if allTools.isEmpty {
                Text(server.health == .working ? "This server offers no tools." : "Tools appear once the server is running.")
                    .foregroundStyle(.secondary)
            } else if shownTools.isEmpty {
                Text(noToolsShown)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(shownTools) { tool in
                    toolRow(tool)
                }
            }
        } header: {
            HStack(spacing: Metric.snug) {
                Text("Tools")
                    .font(PanelType.small.weight(.semibold))
                    .foregroundStyle(.secondary)
                if let toolsSummary {
                    Text(toolsSummary)
                        .font(PanelType.small.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                if !allTools.isEmpty {
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
        }
    }

    private func toolRow(_ tool: ToolFacts) -> some View {
        let canManage = model.canManageTools && model.canMutate
        let isBusy = model.busyTools.contains(tool.name)
        return ToolRow(
            tool: tool,
            canManage: canManage,
            isBusy: isBusy,
            onSelect: { router.selectedTool = tool.name },
            run: run
        )
        .accessibilityAction(named: "Show Details") { router.selectedTool = tool.name }
        .popover(
            isPresented: Binding(
                get: { router.selectedTool == tool.name },
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
