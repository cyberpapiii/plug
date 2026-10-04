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
        ScrollView {
            VStack(alignment: .leading, spacing: Metric.roomy) {
                header
                if server.health == .signInNeeded {
                    signInCard
                } else if let fix = server.fix {
                    problemCard(fix)
                }
                details
                if !recentCalls.isEmpty { recent }
                tools
            }
            .padding(Metric.roomy)
        }
        .scrollBounceBehavior(.basedOnSize)
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
            Text("Apps connected to Plug will stop seeing its tools. Your configuration file keeps everything else.")
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack(alignment: .center, spacing: Metric.snug) {
            StatusGlyph(health: server.health, size: .title2)
            VStack(alignment: .leading, spacing: 1) {
                Text(server.name).font(.title3.weight(.semibold))
                Text(statusLine).font(.callout).foregroundStyle(.secondary)
            }
            Spacer(minLength: Metric.tight)
            HStack(spacing: Metric.tight) {
                if server.enabled {
                    Button("Restart") { run(.restartServer(server.name)) }
                }
                Button("Edit…") { run(.editServer(server.name)) }
                Menu {
                    Button("Add Another Account…") { run(.addAccount(server: server.name)) }
                    if server.usesOAuth, server.health != .signInNeeded {
                        Button("Sign Out…") { confirmSignOut = true }
                    }
                    Button("Remove Server…", role: .destructive) { confirmRemoval = true }
                } label: {
                    Image(systemName: "ellipsis")
                }
                .menuIndicator(.hidden)
                .fixedSize()
                .help("More")
                .accessibilityLabel("More")
            }
            .controlSize(.small)
            Toggle(
                "On",
                isOn: Binding(
                    get: { server.enabled },
                    set: { run(.setServerEnabled(server.name, $0)) }
                )
            )
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.small)
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
            Text("This server needs you to sign in to your account.")
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
        .padding(Metric.regular)
        .nativeInsetSurface(AnyShapeStyle(.orange.opacity(0.1)))
    }

    private func problemCard(_ button: Verdict.Button) -> some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            Text(server.problem).font(.callout.weight(.medium))
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
        .padding(Metric.regular)
        .nativeInsetSurface(AnyShapeStyle(.orange.opacity(0.1)))
    }

    // MARK: - Details

    private var details: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            SectionLabel(text: "Details")
            detailRow("Kind", server.transportLabel, symbol: server.transportSymbol)
            if server.usesOAuth {
                detailRow("Account", accountLabel, symbol: accountSymbol)
            }
            ForEach(server.authWarnings, id: \.self) { warning in
                Label(warning, systemImage: "exclamationmark.circle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func detailRow(_ label: String, _ value: String, symbol: String) -> some View {
        LabeledContent {
            Text(value)
                .font(.callout)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
        } label: {
            Label(label, systemImage: symbol)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    /// The account line's own glyph, so "needs sign-in" is visible before the
    /// words are read.
    private var accountSymbol: String {
        return server.health == .signInNeeded ? "person.badge.key.fill" : "person.badge.shield.checkmark"
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
        VStack(alignment: .leading, spacing: Metric.tight) {
            SectionLabel(text: "Recent calls")
            ForEach(recentCalls) { event in
                HStack(spacing: Metric.tight) {
                    Image(systemName: event.outcome == "success" ? "checkmark" : "exclamationmark.triangle.fill")
                        .font(.caption2)
                        .foregroundStyle(event.outcome == "success" ? Color.secondary : .orange)
                        .frame(width: 12)
                    Text(callName(event))
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: Metric.tight)
                    Text("\(event.latencyMs) ms")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.tertiary)
                }
            }
        }
    }

    /// The tool that ran. `tools/call` is the transport's word for it and says
    /// nothing about what happened, so the tool name wins when there is one.
    private func callName(_ event: ActivityEvent) -> String {
        guard let tool = event.tool, !tool.isEmpty else { return event.method }
        return tool
    }

    // MARK: - Tools

    private var allTools: [ToolFacts] { model.toolCatalog.tools(for: server.name) }
    private var offCount: Int { allTools.filter { !$0.isOn }.count }

    private var shownTools: [ToolFacts] {
        let matched = model.toolCatalog.tools(for: server.name, matching: query)
        return showsOffOnly ? matched.filter { !$0.isOn } : matched
    }

    private var toolsSummary: String {
        let total = allTools.count
        let shown = shownTools.count
        if shown < total, !showsOffOnly { return "\(shown) of \(total)" }
        if offCount > 0 { return "\(total - offCount) of \(total) on" }
        return total == 1 ? "1 tool" : "\(total) tools"
    }

    @ViewBuilder private var tools: some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            HStack(spacing: Metric.snug) {
                SectionLabel(text: "Tools", trailing: allTools.isEmpty ? nil : toolsSummary)
                if offCount > 0 {
                    Picker("Show", selection: $showsOffOnly) {
                        Text("All").tag(false)
                        Text("Off \(offCount)").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .controlSize(.small)
                    .fixedSize()
                }
            }
            if allTools.isEmpty {
                Text(server.health == .working ? "This server offers no tools." : "Tools appear once the server is running.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else if shownTools.isEmpty {
                Text("No tool matches “\(query.trimmingCharacters(in: .whitespaces))”.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(shownTools) { tool in
                        toolRow(tool)
                    }
                }
                // Rows carry their own inset; pull them back so tool names
                // line up with the section label.
                .padding(.horizontal, -Metric.snug)
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
        .padding(.horizontal, Metric.snug)
        .frame(minHeight: tool.summary?.isEmpty == false ? 46 : 36)
        .hoverHighlight(cornerRadius: 7)
        .contentShape(Rectangle())
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
