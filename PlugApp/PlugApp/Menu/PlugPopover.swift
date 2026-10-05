import AppKit
import PlugIPC
import SwiftUI

/// The app.
///
/// Plug is background infrastructure, so almost every visit is one of two
/// questions: "is it working?" and "what do I press to fix it?". This panel
/// answers both without opening a window. The window exists for the rare work —
/// adding a server, auditing connections, reading history — and nothing that
/// belongs here has been moved there.
///
/// Shape of the panel, top to bottom: one headline with its fix, the server
/// list with each fix beside its row, who is connected, the last few tool
/// calls, and the controls. Servers never leave the list when they break, so
/// rows do not jump around.
struct PlugPopover: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dismiss) private var dismiss

    private var situation: PlugSituation { model.situation }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if model.serviceEnabled, !servers.isEmpty {
                Divider()
                serverList
            }
            if model.serviceEnabled, situation.connectedApps > 0 {
                Divider()
                connectedAppsRow
            }
            if model.serviceEnabled, !recentCalls.isEmpty {
                Divider()
                recent
            }
            Divider()
            footer
        }
        .frame(width: Metric.popoverWidth)
        .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: model.verdict)
        .animation(reduceMotion ? nil : .snappy(duration: 0.2), value: servers)
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            HStack {
                Text("Plug").font(.headline)
                Spacer()
                ServicePowerToggle(model: model, run: run)
                    .labelsHidden()
                    .controlSize(.small)
            }
            VerdictView(verdict: heroVerdict, style: .hero, run: send)
            if let error = model.actionError {
                ProblemNote(error) { send(.dismissActionError) }
            }
        }
        .padding(.horizontal, Metric.panelInset)
        .padding(.top, Metric.regular)
        .padding(.bottom, Metric.regular)
    }

    /// The headline without "Turn On" when Plug is off: the switch that does
    /// it is right above.
    private var heroVerdict: Verdict {
        let verdict = model.verdict
        guard verdict.primary?.intent == .setServiceEnabled(true) else { return verdict }
        return Verdict(
            tone: verdict.tone,
            symbol: verdict.symbol,
            title: verdict.title,
            detail: verdict.detail,
            primary: nil,
            secondary: verdict.secondary
        )
    }

    // MARK: - Servers

    /// Every server, on or off, in the order the window lists them. A server
    /// that is off is still a server someone may be looking for.
    private var servers: [ServerFacts] { situation.listedServers }

    private var serverList: some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            SectionLabel(text: "Servers", trailing: model.dataIsStale ? "Last known" : nil)
                .padding(.horizontal, Metric.snug)
                .padding(.top, Metric.snug)

            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(servers) { server in
                        PanelServerRow(
                            server: server,
                            showsFix: situation.troubledServers.count != 1,
                            canFix: model.canMutate,
                            run: send
                        )
                        .frame(height: Metric.popoverRowHeight)
                    }
                }
                .padding(.bottom, Metric.tight)
            }
            // The rows are what the daemon said last, not what it says now.
            .opacity(model.dataIsStale ? 0.55 : 1)
            .frame(height: listHeight)
            .scrollBounceBehavior(.basedOnSize)
        }
        .padding(.horizontal, Metric.tight)
    }

    /// As tall as its rows up to a fixed count. Past that the list scrolls,
    /// and part of the next row shows so the cut reads as more below.
    private var listHeight: CGFloat {
        let rows = min(servers.count, Metric.popoverVisibleRows)
        let partial: CGFloat = servers.count > Metric.popoverVisibleRows ? Metric.popoverRowHeight * 0.45 : 0
        return CGFloat(rows) * Metric.popoverRowHeight + partial + Metric.tight
    }

    // MARK: - Connected clients

    private var connectedAppsRow: some View {
        Button { send(.openWindow(.clients)) } label: {
            HStack(spacing: Metric.snug) {
                AppIconStack(clients: situation.connectedClients)
                Text(connectedAppsText)
                    .font(.callout)
                    .contentTransition(.numericText())
                Spacer(minLength: Metric.tight)
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tertiary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(QuietRowButtonStyle())
        .padding(.horizontal, Metric.tight)
        .padding(.vertical, Metric.tight)
        .help("Show Clients")
    }

    private var connectedAppsText: String {
        let names = situation.connectedClients.map(\.name).filter { !$0.isEmpty }
        if !names.isEmpty, names.count == situation.connectedApps, names.count <= 2 {
            return names.joined(separator: " and ") + " connected"
        }
        return situation.connectedApps == 1 ? "1 client connected" : "\(situation.connectedApps) clients connected"
    }

    // MARK: - Recent

    private var recentCalls: [ActivityEvent] {
        Self.recentCalls(model.activities, limit: 3)
    }

    /// The newest tool calls. Listing, pings, and other protocol traffic are
    /// not what anyone opens the panel to see.
    static func recentCalls(_ activities: [ActivityEvent], limit: Int) -> [ActivityEvent] {
        activities
            .filter { $0.tool?.isEmpty == false }
            .sorted { $0.sequence > $1.sequence }
            .prefix(limit)
            .map { $0 }
    }

    private var recent: some View {
        VStack(alignment: .leading, spacing: Metric.rowGap) {
            Button { send(.openWindow(.activity)) } label: {
                HStack(spacing: Metric.tight) {
                    SectionLabel(text: "Recent Activity")
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(QuietRowButtonStyle())
            .padding(.horizontal, Metric.tight)
            .help("Show All Activity")

            ForEach(recentCalls) { event in
                RecentCallRow(call: CallFacts(event))
                    .padding(.horizontal, Metric.panelInset)
            }
        }
        .padding(.top, Metric.tight)
        .padding(.bottom, Metric.snug)
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: Metric.tight) {
            Button { send(.openCurrentWindow) } label: {
                Label("Open Plug", systemImage: "macwindow")
            }
            Spacer(minLength: 0)
            Button { send(.openSettings) } label: {
                Label("Settings…", systemImage: "gearshape")
            }
            .labelStyle(.iconOnly)
            .keyboardShortcut(",", modifiers: .command)
            .help("Settings")
            Button { run(.quit) } label: {
                Label("Quit Plug", systemImage: "power")
            }
            .labelStyle(.iconOnly)
            .keyboardShortcut("q", modifiers: .command)
            .help("Quit the menu bar app. Plug keeps serving your clients until you turn it off.")
        }
        .buttonStyle(.accessoryBar)
        .padding(.horizontal, Metric.tight)
        .padding(.vertical, Metric.tight)
    }

    /// Window-opening actions close the menu panel first. Otherwise its
    /// floating window can remain above the sheet or inspector it opened.
    private func send(_ intent: PlugIntent) {
        switch intent {
        case .addServer, .importServers, .editServer, .openWindow, .openSettings, .checkup,
             .openCurrentWindow, .reveal, .showRepairLog, .signIn:
            dismiss()
        default:
            break
        }
        run(intent)
    }
}

// MARK: - Rows

/// One server in the panel. Healthy rows are quiet and show their tool count;
/// a troubled row keeps its place and says what is wrong. Its fix sits beside
/// the row, except when it is the only trouble and the headline already
/// offers the same button.
private struct PanelServerRow: View {
    let server: ServerFacts
    let showsFix: Bool
    let canFix: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.tight) {
            Button { run(.reveal(server: server.name)) } label: {
                HStack(spacing: Metric.snug) {
                    ServerGlyph(name: server.name, status: server.enabled ? server.health.color : nil)
                        .opacity(server.enabled ? 1 : 0.4)
                    Text(server.name)
                        .font(.callout)
                        .foregroundStyle(server.enabled ? .primary : .secondary)
                        .lineLimit(1)
                    Spacer(minLength: Metric.tight)
                    if !offersFix {
                        Text(trailingText)
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(QuietRowButtonStyle())
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(server.name), \(server.health.label)")

            if offersFix {
                fixControl
                    .padding(.trailing, Metric.snug)
            }
        }
    }

    private var offersFix: Bool {
        showsFix && server.health.needsAttention && (server.cancelSignIn != nil || server.fix != nil)
    }

    @ViewBuilder private var fixControl: some View {
        if let cancel = server.cancelSignIn {
            ProgressView().controlSize(.small)
            Button(cancel.title) { run(cancel.intent) }
                .controlSize(.small)
                .accessibilityLabel("\(cancel.title), \(server.name)")
        } else if let fix = server.fix {
            Button(fix.title) { run(fix.intent) }
                .controlSize(.small)
                .disabled(!canFix)
                .accessibilityLabel("\(fix.title), \(server.name)")
        }
    }

    private var trailingText: String {
        switch server.health {
        case .working: server.toolCountText
        default: server.health.label
        }
    }
}

/// One tool call: whether it worked, what ran, where, and how long it took.
private struct RecentCallRow: View {
    let call: CallFacts

    var body: some View {
        HStack(spacing: Metric.tight) {
            Image(systemName: symbol)
                .font(.caption2)
                .foregroundStyle(call.failed ? Color.red : .secondary)
                .frame(width: 12)
            Text(call.tool)
                .font(.caption)
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let server = call.server {
                Text(server)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            Text(call.duration)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
                .fixedSize()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(call.tool)\(call.server.map { ", \($0)" } ?? ""), \(call.result), \(call.spokenDuration)"
        )
    }

    private var symbol: String {
        if call.failed { return "xmark.circle.fill" }
        return call.cancelled ? "minus.circle.fill" : "checkmark"
    }
}

/// Up to three connected app icons, so the row says who is connected before
/// the words do.
private struct AppIconStack: View {
    let clients: [ConnectedClient]

    var body: some View {
        HStack(spacing: Metric.rowGap) {
            ForEach(Array(clients.prefix(3).enumerated()), id: \.offset) { _, client in
                AppGlyph(target: client.target, name: client.name, appPath: client.appPath)
            }
        }
        .accessibilityHidden(true)
    }
}
