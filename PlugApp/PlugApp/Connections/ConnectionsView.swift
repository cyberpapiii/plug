import PlugIPC
import SwiftUI

/// Who can use Plug. The old app split this in two — "Clients" listed apps and
/// "Auth" listed the grants for the same apps — so the audit question ("who
/// reaches my tools, and how do I cut them off?") could not be answered in one
/// place. It can now.
struct ConnectionsView: View {
    let model: AppModel
    let run: (PlugIntent) -> Void

    private var sessions: [LiveSession] { model.snapshot.liveSessions }
    private var grants: [DownstreamClient] { model.snapshot.downstreamClients }

    var body: some View {
        Group {
            if sessions.isEmpty && grants.isEmpty && model.connectableApps.isEmpty {
                EmptyPage(
                    title: "Nothing is connected",
                    message: "When an AI app connects through Plug it shows up here, along with everything it can reach.",
                    symbol: "app.connected.to.app.below.fill"
                )
            } else {
                List {
                    if !model.connectableApps.isEmpty {
                        Section {
                            ForEach(model.connectableApps) { app in
                                AppLinkRow(
                                    app: app,
                                    isBusy: model.busyApps.contains(app.target),
                                    run: run
                                )
                            }
                        } header: {
                            Text("Apps on this Mac")
                        } footer: {
                            Text("Turning an app on writes Plug into its settings. It picks up the change the next time it starts.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    if !sessions.isEmpty {
                        Section("Connected now") {
                            ForEach(sessions) { session in
                                sessionRow(session)
                            }
                        }
                    }
                    if !grants.isEmpty {
                        Section {
                            ForEach(grants) { grant in
                                GrantRow(grant: grant, run: run)
                            }
                        } header: {
                            Text("Authorized to connect remotely")
                        } footer: {
                            Text("These have a standing key to reach Plug over the network. Revoke anything you don't recognize.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .listStyle(.inset)
            }
        }
        .navigationTitle("Connections")
        .task { await model.loadConnectableApps() }
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
    }

    private func sessionRow(_ session: LiveSession) -> some View {
        HStack(spacing: Metric.snug) {
            AppGlyph(
                target: AppIcons.target(forClientType: session.clientType),
                name: displayName(session)
            )
            VStack(alignment: .leading, spacing: 0) {
                Text(displayName(session)).font(.body)
                Text(connectionDescription(session))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: Metric.tight)
            Text(toolsText(session))
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, Metric.tight - 2)
        .accessibilityElement(children: .combine)
    }

    private func displayName(_ session: LiveSession) -> String {
        if let info = session.clientInfo, !info.isEmpty { return info }
        return session.clientType
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
    }

    /// Says how it reached Plug in words, not transport identifiers.
    private func connectionDescription(_ session: LiveSession) -> String {
        let how: String
        switch session.transport.lowercased() {
        case "stdio", "ipc": how = "On this Mac"
        case "http", "streamable_http", "sse": how = "Over the network"
        default: how = session.transport.replacingOccurrences(of: "_", with: " ").capitalized
        }
        return "\(how) · \(duration(session.connectedSecs))"
    }

    private func duration(_ seconds: UInt64) -> String {
        if seconds < 60 { return "just connected" }
        if seconds < 3_600 { return "connected \(seconds / 60)m" }
        if seconds < 86_400 { return "connected \(seconds / 3_600)h" }
        return "connected \(seconds / 86_400)d"
    }

    private func toolsText(_ session: LiveSession) -> String {
        let count = model.snapshot.clientVisibility
            .first { $0.sessionId == session.sessionId }?
            .visibleToolCount ?? 0
        return count == 1 ? "1 tool" : "\(count) tools"
    }
}

/// An AI app on this Mac, and whether Plug is wired into it.
private struct AppLinkRow: View {
    let app: LinkableApp
    let isBusy: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.snug) {
            AppGlyph(target: app.target, name: app.name)
                .opacity(app.detected ? 1 : 0.4)
            VStack(alignment: .leading, spacing: 0) {
                Text(app.name)
                    .font(.body)
                    .foregroundStyle(app.detected ? .primary : .secondary)
                Label(status, systemImage: statusSymbol)
                    .font(.caption)
                    .foregroundStyle(app.linked ? Color.green : Color.secondary)
                    .labelStyle(.titleAndIcon)
            }
            Spacer(minLength: Metric.tight)
            if isBusy {
                ProgressView().controlSize(.small)
            } else if app.detected {
                Toggle(
                    "Use Plug",
                    isOn: Binding(
                        get: { app.linked },
                        set: { run($0 ? .linkApp(app.target) : .unlinkApp(app.target)) }
                    )
                )
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
            }
        }
        .padding(.vertical, Metric.tight - 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(app.name), \(status)")
    }

    /// The state as a glyph, so linked and not-linked are told apart before
    /// the sentence is read.
    private var statusSymbol: String {
        guard app.detected else { return "questionmark.app.dashed" }
        guard app.linked else { return "circle" }
        return app.live ? "bolt.fill" : "checkmark.circle.fill"
    }

    private var status: String {
        guard app.detected else { return "Not installed" }
        guard app.linked else { return "Not using Plug" }
        if app.live {
            return app.sessions == 1 ? "Using Plug · 1 session open" : "Using Plug · \(app.sessions) sessions open"
        }
        switch app.transport?.lowercased() {
        case "stdio": return "Using Plug on this Mac"
        case "http": return "Using Plug over the network"
        default: return "Using Plug"
        }
    }
}

private struct GrantRow: View {
    let grant: DownstreamClient
    let run: (PlugIntent) -> Void
    @State private var confirming = false

    var body: some View {
        HStack(spacing: Metric.snug) {
            Image(systemName: "key.horizontal")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 0) {
                Text(grant.clientName).font(.body)
                Text(grant.clientId)
                    .font(.caption.monospaced())
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: Metric.tight)
            Button("Revoke…", role: .destructive) { confirming = true }
                .controlSize(.small)
        }
        .padding(.vertical, Metric.tight - 2)
        .confirmationDialog(
            "Revoke \(grant.clientName)?",
            isPresented: $confirming,
            titleVisibility: .visible
        ) {
            Button("Revoke Access", role: .destructive) { run(.revokeClient(id: grant.clientId)) }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("It loses access immediately and has to ask for permission again.")
        }
    }
}
