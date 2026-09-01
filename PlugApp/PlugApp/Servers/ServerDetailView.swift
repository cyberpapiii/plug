import PlugIPC
import SwiftUI

/// One server, in full. This absorbed the old Auth section: an account that
/// needs signing in belongs to a server, so it is shown and fixed here.
struct ServerDetailView: View {
    let model: AppModel
    let server: ServerFacts
    @Bindable var router: Router
    let run: (PlugIntent) -> Void
    @State private var confirmRemoval = false
    @State private var confirmSignOut = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Metric.roomy) {
                header
                if server.health == .signInNeeded {
                    signInCard
                } else if let problem {
                    problemCard(problem)
                }
                details
                if !recentCalls.isEmpty { recent }
                actions
            }
            .padding(Metric.roomy)
        }
        .scrollBounceBehavior(.basedOnSize)
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
        HStack(alignment: .top, spacing: Metric.snug) {
            StatusGlyph(health: server.health, size: .title2)
            VStack(alignment: .leading, spacing: 1) {
                Text(server.name).font(.title3.weight(.semibold))
                Text(statusLine).font(.callout).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Button {
                router.selectedServer = nil
            } label: {
                Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary)
                    .frame(width: 24, height: 24)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Close details")
            .help("Close details")
        }
    }

    private var statusLine: String {
        switch server.health {
        case .working: server.toolCount == 1 ? "Working · 1 tool" : "Working · \(server.toolCount) tools"
        case .off: "Switched off"
        default: server.health.label
        }
    }

    // MARK: - Problem

    private var problem: Verdict.Button? {
        switch server.health {
        case .down, .unknown: .init("Restart", .restartServer(server.name))
        default: nil
        }
    }

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
            Text(problemHeadline).font(.callout.weight(.medium))
            if let error = server.error, !error.isEmpty, server.health != .signInNeeded {
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

    private var problemHeadline: String {
        switch server.health {
        case .signInNeeded: "This server needs you to sign in to your account."
        default: "Plug couldn't reach this server."
        }
    }

    // MARK: - Details

    private var details: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            SectionLabel(text: "Details")
            detailRow("Kind", server.transportLabel, symbol: server.transportSymbol)
            detailRow(
                "Tools",
                server.health == .working ? "\(server.toolCount)" : "—",
                symbol: "wrench.and.screwdriver"
            )
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

    // MARK: - Actions

    private var actions: some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            SectionLabel(text: "Manage")
            HStack(spacing: Metric.tight) {
                if server.enabled {
                    Button("Restart") { run(.restartServer(server.name)) }
                } else {
                    Button("Turn On") { run(.setServerEnabled(server.name, true)) }
                }
                Button("Edit…") { run(.editServer(server.name)) }
                Menu("More") {
                    if server.enabled {
                        Button("Turn Off") { run(.setServerEnabled(server.name, false)) }
                    }
                    if server.usesOAuth, server.health != .signInNeeded {
                        Button("Sign Out…") { confirmSignOut = true }
                    }
                    Button("Remove Server…", role: .destructive) { confirmRemoval = true }
                }
            }
            .controlSize(.small)
        }
    }
}
