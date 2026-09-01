import PlugIPC
import SwiftUI

/// Who can use Plug. The old app split this in two — "Clients" listed apps and
/// "Auth" listed the grants for the same apps — so the audit question ("who
/// reaches my tools, and how do I cut them off?") could not be answered in one
/// place. It can now.
struct ConnectionsView: View {
    let model: AppModel
    @Binding var search: String
    let run: (PlugIntent) -> Void

    /// Keep rarely used legacy integrations out of the default inventory. If
    /// one is still linked or live, it remains visible so status is never
    /// hidden from the person who needs to act on it.
    private static let secondaryTargets: Set<String> = ["roocode", "goose"]

    private var sessions: [LiveSession] { model.snapshot.liveSessions }
    private var allApps: [LinkableApp] {
        model.connectableApps.filter { app in
            guard app.detected || app.linked || app.live else { return false }
            let isSecondary = Self.secondaryTargets.contains(app.target.lowercased())
            return !isSecondary || app.linked || app.live
        }
    }
    private var apps: [LinkableApp] {
        allApps.filter { matches($0.name) || matches($0.target) }
    }
    private var usingApps: [LinkableApp] { apps.filter(\.linked) }
    private var availableApps: [LinkableApp] { apps.filter { !$0.linked } }
    private var grants: [DownstreamClient] {
        model.snapshot.downstreamClients.filter {
            matches($0.clientName) || matches($0.source) || matches($0.clientId)
        }
    }
    private var unmatchedSessions: [LiveSession] {
        sessions.filter { session in
            let targets = sessionTargets(session)
            return !allApps.contains { app in targets.contains(app.target) }
                && (matches(displayName(session))
                    || matches(session.transport)
                    || matches(session.sessionId))
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            PageHeader("Connections", detail: connectionSummary)

            Group {
                if model.isLoadingInitialData && !model.hasLoadedSnapshot {
                    LoadingPage(message: "Loading connections…")
                } else if model.initialDataUnavailable {
                    UnavailablePage(item: "Connections") { run(.reconnect) }
                } else if isEmpty {
                    EmptyPage(
                        title: search.isEmpty
                            ? (model.connectableAppsError == nil ? "Nothing is connected" : "App scan failed")
                            : "No matching connections",
                        message: search.isEmpty
                            ? (model.connectableAppsError
                                ?? "When an AI app connects through Plug it shows up here, along with everything it can reach.")
                            : "Nothing connected to Plug matches “\(search.trimmingCharacters(in: .whitespaces))”.",
                        symbol: search.isEmpty
                            ? (model.connectableAppsError == nil
                                ? "app.connected.to.app.below.fill"
                                : "exclamationmark.triangle")
                            : "magnifyingglass"
                    )
                } else {
                    List {
                        if model.isLoadingConnectableApps && apps.isEmpty {
                            HStack(spacing: Metric.snug) {
                                ProgressView().controlSize(.small)
                                Text("Looking for AI apps…").foregroundStyle(.secondary)
                            }
                            .listRowSeparator(.hidden)
                        } else if let error = model.connectableAppsError, apps.isEmpty {
                            HStack(spacing: Metric.snug) {
                                Label(error, systemImage: "exclamationmark.triangle")
                                    .font(.callout)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                                    .layoutPriority(1)
                                Spacer(minLength: 0)
                                Button("Try Again") {
                                    Task { await model.loadConnectableApps() }
                                }
                                .controlSize(.small)
                            }
                            .listRowSeparator(.hidden)
                        }
                        appSection("Using Plug", apps: usingApps)
                        appSection("Available apps", apps: availableApps)
                        if !apps.isEmpty {
                            Text("Turn on an app to add Plug to its settings. Restart that app to pick up the change.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .padding(.bottom, Metric.regular)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                        }
                        if !unmatchedSessions.isEmpty {
                            SectionLabel(
                                text: "Other connections",
                                trailing: unmatchedSessions.count == 1
                                    ? "1 connection"
                                    : "\(unmatchedSessions.count) connections"
                            )
                                .padding(.top, Metric.regular)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                            ForEach(unmatchedSessions) { session in
                                sessionRow(session)
                                    .listRowSeparator(.hidden)
                            }
                        }
                        if !grants.isEmpty {
                            SectionLabel(
                                text: "Remote access",
                                trailing: grants.count == 1 ? "1 client" : "\(grants.count) clients"
                            )
                                .padding(.top, Metric.regular)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                            ForEach(grants) { grant in
                                GrantRow(grant: grant, run: run)
                                    .listRowSeparator(.hidden)
                            }
                            Text("These clients can reach Plug over the network. Revoke anything you don't recognize.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                        }
                    }
                    .listStyle(.inset)
                    .frame(maxWidth: Metric.contentMaxWidth)
                    .frame(maxWidth: .infinity)
                }
            }
        }
        .task { await model.loadConnectableApps() }
    }

    private var isEmpty: Bool {
        unmatchedSessions.isEmpty && grants.isEmpty && apps.isEmpty
            && !model.isLoadingConnectableApps
    }

    @ViewBuilder
    private func appSection(_ title: String, apps: [LinkableApp]) -> some View {
        if !apps.isEmpty {
            SectionLabel(
                text: title,
                trailing: apps.count == 1 ? "1 app" : "\(apps.count) apps"
            )
            .padding(.top, Metric.regular)
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
            ForEach(apps) { app in
                AppLinkRow(
                    app: app,
                    isBusy: model.busyApps.contains(app.target),
                    run: run
                )
                .listRowSeparator(.hidden)
            }
        }
    }

    private func matches(_ value: String) -> Bool {
        let query = search.trimmingCharacters(in: .whitespaces)
        return query.isEmpty || value.localizedCaseInsensitiveContains(query)
    }

    private var connectionSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = sessions.count
        let summary = "\(count) open \(count == 1 ? "session" : "sessions")"
        return model.dataIsStale ? "Last known · \(summary)" : summary
    }

    private func sessionTargets(_ session: LiveSession) -> Set<String> {
        var targets = [AppIcons.target(forClientType: session.clientType)]
        if let info = session.clientInfo, !info.isEmpty {
            targets.append(AppIcons.target(forClientType: info))
        }
        return Set(targets)
    }

    private func sessionRow(_ session: LiveSession) -> some View {
        HStack(spacing: Metric.snug) {
            AppGlyph(
                target: AppIcons.target(forClientType: session.clientType),
                name: displayName(session)
            )
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(displayName(session)).font(.body)
                Text(connectionDescription(session))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            Text(toolsText(session))
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, Metric.snug)
        .accessibilityElement(children: .combine)
    }

    private func displayName(_ session: LiveSession) -> String {
        for value in [session.clientType, session.clientInfo].compactMap({ $0 }) {
            let target = AppIcons.target(forClientType: value)
            if let canonical = AppIcons.displayName(forTarget: target) {
                return canonical
            }
        }
        if let info = session.clientInfo,
           !info.isEmpty,
           info.localizedCaseInsensitiveCompare("mcp") != .orderedSame
        {
            return info
        }
        let type = session.clientType
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
        guard type.localizedCaseInsensitiveCompare("unknown") == .orderedSame else { return type }
        return "Unidentified local client \(session.sessionId.prefix(4))"
    }

    /// Says how it reached Plug in words, not transport identifiers.
    private func connectionDescription(_ session: LiveSession) -> String {
        let how: String
        switch session.transport.lowercased() {
        case "stdio", "ipc", "daemon_proxy": how = "On this Mac"
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
        guard let count = model.snapshot.clientVisibility
            .first(where: { $0.sessionId == session.sessionId })?
            .visibleToolCount
        else { return "—" }
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
                .opacity(app.detected || app.linked ? 1 : 0.4)
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(app.name)
                    .font(.body)
                    .foregroundStyle(app.detected || app.linked ? .primary : .secondary)
                Label(status, systemImage: statusSymbol)
                    .font(.caption)
                    .foregroundStyle(app.linked ? Color.green : Color.secondary)
                    .labelStyle(.titleAndIcon)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            if isBusy {
                ProgressView().controlSize(.small)
            } else if app.detected || app.linked {
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
                .disabled(!app.detected && !app.linked)
            }
        }
        .padding(.vertical, Metric.snug)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(app.name), \(status)")
    }

    /// The state as a glyph, so linked and not-linked are told apart before
    /// the sentence is read.
    private var statusSymbol: String {
        guard app.linked else { return app.detected ? "circle" : "questionmark.app.dashed" }
        return app.live ? "bolt.fill" : "checkmark.circle.fill"
    }

    private var status: String {
        guard app.linked else { return app.detected ? "Available on this Mac" : "Not installed" }
        if !app.detected { return "Configured · app not found" }
        if app.live {
            return app.sessions == 1 ? "Connected now · 1 session" : "Connected now · \(app.sessions) sessions"
        }
        switch app.transport?.lowercased() {
        case "stdio": return "Ready to use Plug on this Mac"
        case "http": return "Ready to use Plug over the network"
        default: return "Ready to use Plug"
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
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(grant.clientName).font(.body)
                Text(grantDetail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            Button("Revoke…", role: .destructive) { confirming = true }
                .controlSize(.small)
        }
        .padding(.vertical, Metric.snug)
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

    private var grantDetail: String {
        let source = grant.source.trimmingCharacters(in: .whitespacesAndNewlines)
        let identity = source.isEmpty ? "ID \(grant.clientId.prefix(8))" : source
        return identity
    }
}
