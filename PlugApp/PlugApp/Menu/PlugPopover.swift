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
/// Shape of the panel, top to bottom: one headline with its fix, a thin
/// progress line while servers settle, the server list with trouble pinned to
/// the top and its fix inline, who is connected, the last few tool calls, and
/// the controls. Servers never leave the list when they break, so rows do not
/// jump around.
struct PlugPopover: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openSettings) private var openSettings

    private var situation: PlugSituation { model.situation }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if !servers.isEmpty {
                PanelDivider()
                serverList
            }
            if situation.connectedApps > 0 {
                PanelDivider()
                connectedAppsRow
            }
            if !recentCalls.isEmpty {
                PanelDivider()
                recent
            }
            PanelDivider()
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
            VerdictView(verdict: model.verdict, style: .hero, run: send)
            if let error = model.actionError {
                HStack(alignment: .firstTextBaseline, spacing: Metric.tight) {
                    Label(error.message, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.red)
                        .lineLimit(3)
                    Spacer(minLength: 0)
                    Button("Dismiss") { send(.dismissActionError) }
                        .buttonStyle(.link)
                        .font(.caption)
                }
                .accessibilityElement(children: .contain)
            }
            if let progress = settlingProgress {
                ProgressView(value: progress)
                    .progressViewStyle(.linear)
                    .controlSize(.small)
                    .tint(.secondary)
                    .animation(reduceMotion ? nil : .smooth(duration: 0.4), value: progress)
                    .accessibilityLabel("Servers starting")
            }
        }
        .popoverInset()
        .padding(.top, Metric.regular)
        .padding(.bottom, Metric.regular)
    }

    /// Fraction of enabled servers that are up, only while some are still
    /// starting. Nothing else earns a progress bar.
    private var settlingProgress: Double? {
        let active = situation.activeServers
        guard active.contains(where: \.health.isSettling), !active.isEmpty else { return nil }
        return Double(situation.workingServers.count) / Double(active.count)
    }

    // MARK: - Servers

    private var servers: [ServerFacts] { situation.activeServers }

    private var serverList: some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            HStack(spacing: Metric.tight) {
                Text("Servers")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                Text(Self.serversSummary(situation, stale: model.dataIsStale))
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.tertiary)
                    .contentTransition(.numericText())
            }
            .padding(.horizontal, Metric.snug)
            .padding(.top, Metric.snug)
            .accessibilityAddTraits(.isHeader)

            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(servers) { server in
                        PanelServerRow(server: server, run: send)
                            .frame(height: Metric.popoverRowHeight)
                    }
                }
                .padding(.horizontal, Metric.tight)
                .padding(.bottom, Metric.tight)
            }
            // The rows are what the daemon said last, not what it says now.
            .opacity(model.dataIsStale ? 0.55 : 1)
            .frame(height: listHeight)
            .scrollBounceBehavior(.basedOnSize)
            .scrollFade(enabled: servers.count > Metric.popoverVisibleRows)
        }
        .padding(.horizontal, Metric.tight)
    }

    /// Whole rows only. A row cut in half reads as a rendering bug, so the
    /// list is as tall as its rows up to a fixed count and then scrolls.
    private var listHeight: CGFloat {
        let rows = min(servers.count, Metric.popoverVisibleRows)
        let partial: CGFloat = servers.count > Metric.popoverVisibleRows ? Metric.popoverRowHeight * 0.45 : 0
        return CGFloat(rows) * Metric.popoverRowHeight + partial + Metric.tight
    }

    /// Stale rows say so, the way the Servers page does, so a list read
    /// before the daemon went away is not taken for the current state.
    static func serversSummary(_ situation: PlugSituation, stale: Bool) -> String {
        let working = situation.workingServers.count
        let total = situation.activeServers.count
        let summary: String
        if working == total {
            let tools = situation.totalTools
            summary = tools == 1 ? "1 tool" : "\(tools) tools"
        } else {
            summary = "\(working) of \(total) running"
        }
        return stale ? "Last known · \(summary)" : summary
    }

    // MARK: - Connected apps

    private var connectedAppsRow: some View {
        Button { send(.openWindow(.apps)) } label: {
            HStack(spacing: Metric.snug) {
                AppIconStack(targets: situation.connectedAppTargets)
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
        .help("See connected apps")
    }

    private var connectedAppsText: String {
        let names = situation.connectedAppTargets.compactMap(AppIcons.displayName(forTarget:))
        if !names.isEmpty, names.count == situation.connectedApps, names.count <= 2 {
            return names.joined(separator: " and ") + " connected"
        }
        return situation.connectedApps == 1 ? "1 app connected" : "\(situation.connectedApps) apps connected"
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

    /// Tool names arrive as `Server__tool`. The row states the server once, in
    /// words, so the tool half stands alone.
    static func callParts(_ event: ActivityEvent) -> (server: String?, tool: String) {
        let tool = event.tool ?? event.method
        guard let range = tool.range(of: "__"), range.lowerBound > tool.startIndex,
              range.upperBound < tool.endIndex
        else { return (event.server, tool) }
        return (String(tool[..<range.lowerBound]), String(tool[range.upperBound...]))
    }

    private var recent: some View {
        Button { send(.openWindow(.activity)) } label: {
            VStack(alignment: .leading, spacing: Metric.tight) {
                HStack(spacing: Metric.tight) {
                    Text("Recent")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
                ForEach(recentCalls) { event in
                    RecentCallRow(event: event, parts: Self.callParts(event))
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(QuietRowButtonStyle())
        .padding(.horizontal, Metric.tight)
        .padding(.vertical, Metric.tight)
        .help("See all activity")
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: Metric.tight) {
            Button { send(.addServer) } label: {
                Label("Add Server", systemImage: "plus")
            }
            .buttonStyle(QuietControlButtonStyle())
            .help("Add a server")

            Button { send(.openCurrentWindow) } label: {
                Label("Open Plug", systemImage: "macwindow")
            }
            .buttonStyle(QuietControlButtonStyle())
            .help("Open the Plug window")

            Spacer(minLength: 0)

            // Settings and Quit are what people look for in a menu bar app, so
            // they are visible controls rather than entries inside a menu. Both
            // are icon-only: the picture is the label, and the tooltip and the
            // accessibility label carry the words.
            Button {
                dismiss()
                openSettings()
            } label: {
                Image(systemName: "gearshape")
            }
            .buttonStyle(QuietControlButtonStyle(iconOnly: true))
            .help("Settings")
            .accessibilityLabel("Settings")

            Button { run(.quit) } label: {
                Image(systemName: "power")
            }
            .buttonStyle(QuietControlButtonStyle(iconOnly: true))
            .help("Quit Plug. Your servers keep running.")
            .accessibilityLabel("Quit Plug")
        }
        .padding(.horizontal, Metric.snug)
        .padding(.vertical, Metric.tight)
    }

    /// Window-opening actions close the menu panel first. Otherwise its
    /// floating window can remain above the sheet or inspector it opened.
    private func send(_ intent: PlugIntent) {
        switch intent {
        case .addServer, .importServers, .editServer, .openWindow,
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
/// a troubled row keeps its place, turns its trailing text into the problem,
/// and carries the button that fixes it. A chevron appears on hover so the
/// row reads as something you can open.
private struct PanelServerRow: View {
    let server: ServerFacts
    let run: (PlugIntent) -> Void
    @State private var hovering = false

    var body: some View {
        Button { run(.reveal(server: server.name)) } label: {
            HStack(spacing: Metric.snug) {
                StatusGlyph(health: server.health)
                Text(server.name)
                    .font(.callout)
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                Spacer(minLength: Metric.tight)
                trailing
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(QuietRowButtonStyle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(server.name), \(server.health.label)")
        // Combining the row hides the button inside it from VoiceOver, so
        // the same fix is offered as a named action on the row.
        .accessibilityActions {
            if let cancel = server.cancelSignIn {
                Button(cancel.title) { run(cancel.intent) }
            } else if server.health.needsAttention, let fix = server.fix {
                Button(fix.title) { run(fix.intent) }
            }
        }
    }

    @ViewBuilder private var trailing: some View {
        if server.health.needsAttention {
            if let cancel = server.cancelSignIn {
                HStack(spacing: Metric.tight) {
                    ProgressView().controlSize(.mini)
                    Button(cancel.title) { run(cancel.intent) }
                        .buttonStyle(.bordered)
                        .controlSize(.mini)
                }
            } else if let fix = server.fix {
                Button(fix.title) { run(fix.intent) }
                    .buttonStyle(.bordered)
                    .controlSize(.mini)
                    .tint(server.health.color)
            }
        } else {
            ZStack(alignment: .trailing) {
                Text(trailingText)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .opacity(hovering ? 0 : 1)
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .opacity(hovering ? 1 : 0)
            }
            .animation(.easeOut(duration: 0.12), value: hovering)
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
    let event: ActivityEvent
    let parts: (server: String?, tool: String)

    private var succeeded: Bool { event.outcome == "success" }

    var body: some View {
        HStack(spacing: Metric.tight) {
            Image(systemName: succeeded ? "checkmark" : "exclamationmark.triangle.fill")
                .font(.caption2)
                .foregroundStyle(succeeded ? Color.secondary : .orange)
                .frame(width: 12)
            Text(parts.tool)
                .font(.caption.monospaced())
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.middle)
            if let server = parts.server {
                Text(server)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .layoutPriority(-1)
            }
            Spacer(minLength: Metric.tight)
            Text("\(event.latencyMs) ms")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(parts.tool)\(parts.server.map { ", \($0)" } ?? ""), \(succeeded ? "succeeded" : "failed"), \(event.latencyMs) milliseconds"
        )
    }
}

/// Up to three connected app icons, overlapped like a short stack, so the row
/// says who is connected before the words do.
private struct AppIconStack: View {
    let targets: [String]

    var body: some View {
        HStack(spacing: -6) {
            ForEach(Array(targets.prefix(3).enumerated()), id: \.offset) { index, target in
                AppGlyph(
                    target: target,
                    name: AppIcons.displayName(forTarget: target) ?? "",
                    size: 18
                )
                .background(
                    Circle().fill(.background).padding(-1.5)
                )
                .zIndex(Double(3 - index))
            }
        }
        .frame(minWidth: 18)
        .accessibilityHidden(true)
    }
}

/// A hairline that separates the panel's blocks without boxing them.
private struct PanelDivider: View {
    var body: some View {
        Rectangle()
            .fill(.separator)
            .frame(height: 1)
            .opacity(0.6)
    }
}

/// A text-or-icon control that sits flat until hovered. The panel's footer
/// needs four controls to be obvious without four separate pills.
private struct QuietControlButtonStyle: ButtonStyle {
    var iconOnly = false
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.callout)
            .labelStyle(.titleAndIcon)
            .foregroundStyle(configuration.isPressed ? .primary : .secondary)
            .padding(.horizontal, iconOnly ? Metric.tight : Metric.snug)
            .frame(height: 28)
            .frame(minWidth: iconOnly ? 28 : 0)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.12 : (hovering ? 0.07 : 0)))
            )
            .contentShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
            .animation(.easeOut(duration: 0.12), value: hovering)
            .onHover { hovering = $0 }
    }
}

private extension View {
    /// Fades the last rows of a scrolling list so the cut-off reads as
    /// "more below" instead of a clipped row.
    @ViewBuilder
    func scrollFade(enabled: Bool) -> some View {
        if enabled {
            mask(
                VStack(spacing: 0) {
                    Color.black
                    LinearGradient(colors: [.black, .clear], startPoint: .top, endPoint: .bottom)
                        .frame(height: 22)
                }
            )
        } else {
            self
        }
    }
}
