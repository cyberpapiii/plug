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
    @State private var confirmRemoval = false
    @State private var confirmSignOut = false
    @State private var showsOffOnly = false

    var body: some View {
        DetailForm {
            Section {
                header
                if server.health == .signInNeeded {
                    signInCard
                } else if let fix = server.fix {
                    problemCard(fix)
                }
            }
            details
            if !recentCalls.isEmpty { recent }
            tools
        }
        .onChange(of: offCount) { _, count in
            if count == 0 { showsOffOnly = false }
        }
        .confirmationDialog(
            "Sign out of \(server.name)?",
            isPresented: $confirmSignOut,
            titleVisibility: .visible
        ) {
            Button("Sign Out", role: .destructive) { run(.signOut(server: server.name)) }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Plug forgets the stored account. The server stops working until you sign in again.")
        }
        .confirmationDialog(
            "Remove \(server.name)?",
            isPresented: $confirmRemoval,
            titleVisibility: .visible
        ) {
            Button("Remove Server", role: .destructive) { run(.removeServer(server.name)) }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Clients connected to Plug will stop seeing its tools. Your settings file keeps everything else.")
        }
    }

    // MARK: - Header

    private var header: some View {
        DetailHeader(title: server.name, subtitle: statusLine) {
            StatusGlyph(health: server.health, size: .title2)
        } controls: {
            ControlGroup {
                if server.enabled {
                    Button("Restart") { run(.restartServer(server.name)) }
                }
                Button("Edit…") { run(.editServer(server.name)) }
            }
            .fixedSize()
            Menu {
                Button("Add Another Account…") { run(.addAccount(server: server.name)) }
                if server.usesOAuth, server.health != .signInNeeded {
                    Button("Sign Out…") { confirmSignOut = true }
                }
                Divider()
                Button("Remove Server…", role: .destructive) { confirmRemoval = true }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("More")
            .accessibilityLabel("More")
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
            .accessibilityLabel("\(server.name) on")
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

    private var signInCard: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            Label("This server needs you to sign in to your account.", systemImage: "exclamationmark.triangle.fill")
                .symbolRenderingMode(.multicolor)
                .font(.callout.weight(.medium))
            if server.isSigningIn {
                HStack(spacing: Metric.tight) {
                    ProgressView().controlSize(.small)
                    Text("Finish signing in in your browser.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack(spacing: Metric.tight) {
                    Button("Try Again") { run(.signIn(server: server.name)) }
                    Button("Cancel") { run(.cancelSignIn(server: server.name)) }
                }
                .controlSize(.small)
            } else {
                Button("Sign In") { run(.signIn(server: server.name)) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, Metric.rowGap)
    }

    private func problemCard(_ button: Verdict.Button) -> some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            Label(server.problem, systemImage: "exclamationmark.triangle.fill")
                .symbolRenderingMode(.multicolor)
                .font(.callout.weight(.medium))
            if let error = server.error, !error.isEmpty {
                Text(error)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .lineLimit(6)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button(button.title) { run(button.intent) }
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, Metric.rowGap)
    }

    // MARK: - Details

    private var details: some View {
        Section("Details") {
            LabeledContent("Kind", value: server.transportLabel)
            if server.usesOAuth {
                LabeledContent("Account", value: accountLabel)
            }
            ForEach(server.authWarnings, id: \.self) { warning in
                Label(warning, systemImage: "exclamationmark.circle")
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var accountLabel: String {
        switch server.health {
        case .signInNeeded: return "Sign-in needed"
        default: break
        }
        guard let seconds = server.tokenExpiresInSecs else { return "Signed in" }
        if seconds >= 86_400 { return "Signed in · renews in \(seconds / 86_400)d" }
        if seconds >= 3_600 { return "Signed in · renews in \(seconds / 3_600)h" }
        return "Signed in · renews shortly"
    }

    // MARK: - Recent

    private var recentCalls: [ActivityEvent] {
        model.recentActivity(for: server.name, limit: 6)
    }

    private var recent: some View {
        Section("Recent Calls") {
            ForEach(recentCalls) { event in
                let call = CallFacts(event)
                HStack(spacing: Metric.tight) {
                    Text(call.tool)
                        .font(.callout.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: Metric.tight)
                    if !call.succeeded {
                        Label(call.result, systemImage: "exclamationmark.triangle.fill")
                            .font(.callout)
                            .foregroundStyle(.orange)
                            .lineLimit(1)
                    }
                    Text(call.duration)
                        .font(.callout.monospacedDigit())
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

    private var tools: some View {
        Section {
            if allTools.isEmpty {
                Text(server.health == .working ? "This server offers no tools." : "Tools appear once the server is running.")
                    .foregroundStyle(.secondary)
            } else if shownTools.isEmpty {
                Text("No tool matches “\(query.trimmingCharacters(in: .whitespaces))”.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(shownTools) { tool in
                    toolRow(tool)
                }
            }
        } header: {
            HStack(spacing: Metric.snug) {
                Text("Tools")
                if let toolsSummary {
                    Text(toolsSummary)
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                if offCount > 0 {
                    Picker("Show", selection: $showsOffOnly) {
                        Text("All").tag(false)
                        Text("Off (\(offCount))").tag(true)
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
        .accessibilityAction(named: "Show details") { router.selectedTool = tool.name }
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
                router: router,
                run: run
            )
            .frame(width: 320)
            .frame(maxHeight: 420)
        }
    }
}
