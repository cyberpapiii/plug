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
/// Shape of the panel, top to bottom: Plug's character with the one line Plug
/// says and the switch; a card for whatever needs you, with its fix at the end
/// of its row; the servers that are fine, as a shelf of icons; who is
/// connected; the last tool call; and the controls. When Plug is off, stopped,
/// or has no servers, everything under the card folds away.
struct PlugPopover: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dismiss) private var dismiss

    private var situation: PlugSituation { model.situation }
    private var trouble: PanelTrouble? {
        model.serviceEnabled ? PanelTrouble.trouble(for: situation, verdict: model.verdict) : nil
    }

    /// Off, stopped, or with no servers, there is nothing under the card to
    /// show.
    private var showsBody: Bool {
        model.serviceEnabled && situation.setup == .ready && situation.runtime != .stopped
            && situation.runtime != .off && !situation.activeServers.isEmpty
    }

    var body: some View {
        let trouble = trouble
        VStack(alignment: .leading, spacing: 0) {
            header(trouble)
            if let error = model.actionError {
                ProblemNote(error) { send(.dismissActionError) }
                    .padding(.horizontal, Metric.panelInset)
                    .padding(.bottom, Metric.snug)
            }
            if let trouble {
                TroubleCard(trouble: trouble, canFix: model.canMutate, run: send)
                    .padding(.horizontal, Metric.tight + Metric.hairline)
                    .padding(.bottom, Metric.snug)
                    .transition(.opacity)
            }
            if showsBody {
                VStack(alignment: .leading, spacing: 0) {
                    shelf
                    clientsLine
                    activityLine
                }
                // The rows are what the daemon said last, not what it says now.
                .opacity(model.dataIsStale ? 0.55 : 1)
                .padding(.bottom, Metric.tight)
                .transition(.opacity)
            }
            Divider()
            footer
        }
        .frame(width: Metric.popoverWidth)
        .animation(reduceMotion ? nil : .snappy(duration: 0.25), value: model.verdict)
        .animation(reduceMotion ? nil : .snappy(duration: 0.25), value: trouble)
        .animation(reduceMotion ? nil : .snappy(duration: 0.25), value: situation.settledServers)
        .animation(reduceMotion ? nil : .snappy(duration: 0.3), value: latestCall?.sequence)
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
    }

    // MARK: - Header

    /// The character is always Plug blue. Its face says how Plug is, and the
    /// small mark beside it takes the colour of the trouble.
    private func header(_ trouble: PanelTrouble?) -> some View {
        let verdict = model.verdict
        return HStack(spacing: Metric.snug + Metric.hairline) {
            PlugCharacter(mood: verdict.mood, mark: trouble?.wash?.color ?? StatusColor.needsYou)
                .foregroundStyle(.tint)
                .frame(width: Self.characterSize, height: Self.characterSize)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(verdict.title)
                    .font(.system(size: 15, weight: .semibold))
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail = headerDetail {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            Spacer(minLength: Metric.tight)
            ServicePowerToggle(model: model, run: run)
                .labelsHidden()
                .controlSize(.small)
        }
        .padding(.horizontal, Metric.panelInset)
        .padding(.top, Metric.panelInset)
        .padding(.bottom, trouble == nil && !showsBody ? Metric.panelInset : Metric.snug + Metric.hairline)
    }

    private static let characterSize: CGFloat = 40

    /// A second line only while Plug itself is on its way up. Trouble says
    /// more in its card, and all-is-well needs no more said.
    private var headerDetail: String? {
        let verdict = model.verdict
        guard verdict.tone == .busy, situation.setup != .ready || situation.runtime != .running else { return nil }
        return verdict.detail
    }

    // MARK: - Servers

    /// The servers with nothing wrong, as icons. Past two rows the rest are a
    /// number: the window lists them all.
    private var shelf: some View {
        let servers = situation.settledServers
        let shown = servers.count > Self.shelfLimit ? Array(servers.prefix(Self.shelfLimit - 1)) : servers
        return VStack(alignment: .leading, spacing: Metric.hairline) {
            if !servers.isEmpty {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: Self.tileSize, maximum: Self.tileSize), spacing: Metric.tight + Metric.hairline)],
                    alignment: .leading,
                    spacing: Metric.tight + Metric.hairline
                ) {
                    ForEach(shown) { server in
                        Button { send(.reveal(server: server.name)) } label: {
                            ServerGlyph(name: server.name, size: Self.tileSize, status: server.health.color)
                                .opacity(server.health == .working ? 1 : 0.5)
                        }
                        .buttonStyle(.plain)
                        .help("\(server.name) · \(server.health == .working ? server.toolCountText : server.health.label)")
                        .accessibilityLabel("\(server.name), \(server.health.label)")
                        .transition(.scale(scale: 0.6).combined(with: .opacity))
                    }
                    if shown.count < servers.count {
                        Text("+\(servers.count - shown.count)")
                            .font(.caption2.weight(.semibold).monospacedDigit())
                            .foregroundStyle(.secondary)
                            .frame(width: Self.tileSize, height: Self.tileSize)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: Metric.smallCorner, style: .continuous))
                            .accessibilityLabel("\(servers.count - shown.count) more servers")
                    }
                }
                .padding(.horizontal, Metric.panelInset)
                .padding(.bottom, Metric.rowGap)
            }
            Button { send(.openWindow(.servers)) } label: {
                HStack(spacing: Metric.rowGap) {
                    Text(situation.runningSummary(stale: model.dataIsStale))
                        .foregroundStyle(.secondary)
                        .contentTransition(.numericText())
                    Spacer(minLength: Metric.tight)
                    Text("Show all").foregroundStyle(.secondary)
                    chevron
                }
                .font(.caption)
                .contentShape(Rectangle())
            }
            .buttonStyle(QuietRowButtonStyle())
            .padding(.horizontal, Metric.tight)
            .help("Show Servers")
        }
    }

    private static let tileSize: CGFloat = 24
    /// Two rows of the panel's width.
    private static let shelfLimit = 18

    private var chevron: some View {
        Image(systemName: "chevron.right")
            .font(.caption2.weight(.semibold))
            .foregroundStyle(.tertiary)
    }

    // MARK: - Connected clients

    private var clientsLine: some View {
        Button { send(.openWindow(.clients)) } label: {
            HStack(spacing: Metric.tight + Metric.hairline) {
                if !situation.connectedClients.isEmpty {
                    AppIconStack(clients: situation.connectedClients)
                }
                Text(connectedAppsText)
                    .font(.caption)
                    .foregroundStyle(situation.connectedApps == 0 ? Color.secondary : Color.primary)
                    .lineLimit(1)
                    .contentTransition(.numericText())
                Spacer(minLength: Metric.tight)
                chevron
            }
            .frame(minHeight: AppIconStack.size)
            .contentShape(Rectangle())
        }
        .buttonStyle(QuietRowButtonStyle())
        .padding(.horizontal, Metric.tight)
        .help("Show Clients")
    }

    private var connectedAppsText: String {
        let names = situation.connectedClients.map(\.name).filter { !$0.isEmpty }
        if !names.isEmpty, names.count == situation.connectedApps, names.count <= 2 {
            return names.joined(separator: " and ") + " connected"
        }
        switch situation.connectedApps {
        case 0: return "No clients connected"
        case 1: return "1 client connected"
        default: return "\(situation.connectedApps) clients connected"
        }
    }

    // MARK: - Activity

    private var latestCall: ActivityEvent? {
        Self.recentCalls(model.activities, limit: 1).first
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

    /// The last tool call, on one line. A new one rolls in from below.
    private var activityLine: some View {
        Button { send(.openWindow(.activity)) } label: {
            HStack(spacing: Metric.tight + Metric.hairline) {
                Group {
                    if let event = latestCall {
                        LatestCallLine(call: model.call(event))
                            .id(event.sequence)
                            .transition(.push(from: .bottom))
                    } else {
                        LatestCallLine(call: nil)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .clipped()
                chevron
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(QuietRowButtonStyle())
        .padding(.horizontal, Metric.tight)
        .help("Show All Activity")
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
            .help("Quit Plug")
        }
        .buttonStyle(.accessoryBar)
        .padding(.horizontal, Metric.tight)
        .padding(.vertical, Metric.tight)
    }

    /// Window-opening actions close the menu panel first. Otherwise its
    /// floating window can remain above the sheet or inspector it opened.
    private func send(_ intent: PlugIntent) {
        switch intent {
        case .addServer, .addClient, .importServers, .editServer, .openWindow, .openSettings, .checkup,
             .openCurrentWindow, .reveal, .showRepairLog, .signIn:
            dismiss()
        default:
            break
        }
        run(intent)
    }
}

// MARK: - Trouble

/// What the panel's card says: the servers that need something, and one plain
/// line with the thing to press. Worked out from the situation alone, so tests
/// can pin it.
struct PanelTrouble: Equatable {
    /// A row each for this many servers; the rest are counted.
    static let visibleServers = 3

    var servers: [ServerFacts] = []
    var note: String?
    var icon: PanelIcon.Kind?
    /// The one button to press, in Plug blue.
    var primary: Verdict.Button?
    var secondary: Verdict.Button?
    /// The colour washed behind the card: how bad the worst thing in it is.
    /// Nil for a card that only invites, such as adding a first server.
    var wash: Verdict.Tone?

    /// One server in trouble is the one thing to press, so its fix is blue.
    /// With several, no fix outranks another.
    var fixIsPrimary: Bool { servers.count == 1 && primary == nil }

    static func trouble(for situation: PlugSituation, verdict: Verdict) -> PanelTrouble? {
        guard situation.setup == .ready else { return general(verdict) }
        switch situation.runtime {
        case .running:
            return servers(situation, verdict: verdict)
        case .stopped:
            return PanelTrouble(
                note: "Clients cannot reach servers", icon: .plug, primary: verdict.primary, wash: .blocked
            )
        default:
            return general(verdict)
        }
    }

    private static func servers(_ situation: PlugSituation, verdict: Verdict) -> PanelTrouble? {
        if situation.activeServers.isEmpty {
            return PanelTrouble(note: "Add your first one", icon: .addServer, primary: verdict.primary)
        }
        let troubled = situation.troubledServers
        guard !troubled.isEmpty else { return nil }
        let stopped = troubled.contains { $0.health == .down || $0.health == .unknown }
        var card = PanelTrouble(
            servers: Array(troubled.prefix(visibleServers)), wash: stopped ? .blocked : .attention
        )
        guard troubled.count > 1 else { return card }
        let running = "\(situation.workingServers.count) of \(situation.activeServers.count) servers running"
        let hidden = troubled.count - card.servers.count
        card.note = hidden > 0 ? "and \(hidden) more · \(running)" : running
        card.icon = .checkup
        card.secondary = .init("Run Checkup", .checkup)
        return card
    }

    /// Plug itself needs something: permission, repair, a restart.
    private static func general(_ verdict: Verdict) -> PanelTrouble? {
        guard let primary = verdict.primary, verdict.tone == .attention || verdict.tone == .blocked else {
            return nil
        }
        return PanelTrouble(
            note: verdict.detail,
            icon: primary.intent == .repairInstallation ? .checkup : .plug,
            primary: primary,
            secondary: verdict.secondary,
            wash: verdict.tone
        )
    }
}

private struct TroubleCard: View {
    let trouble: PanelTrouble
    let canFix: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(trouble.servers) { server in
                TroubleRow(server: server, prominent: trouble.fixIsPrimary, canFix: canFix, run: run)
            }
            if trouble.note != nil || trouble.primary != nil {
                if !trouble.servers.isEmpty {
                    Divider().padding(.vertical, Metric.rowGap)
                }
                summary
            }
        }
        .padding(.horizontal, Metric.tight + Metric.hairline)
        .padding(.vertical, Metric.rowGap)
        .background(
            RoundedRectangle(cornerRadius: Metric.corner, style: .continuous)
                .fill(trouble.wash.map { $0.color.opacity(StatusColor.wash) } ?? .clear)
        )
    }

    /// One plain line: a small picture, a few words, and what to press.
    private var summary: some View {
        HStack(spacing: Metric.snug) {
            if let icon = trouble.icon {
                PanelIcon(kind: icon)
                    .frame(width: 24, height: 24)
            }
            if let note = trouble.note {
                Text(note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: Metric.tight)
            if let secondary = trouble.secondary {
                Button(secondary.title) { run(secondary.intent) }
            }
            if let primary = trouble.primary {
                Button(primary.title) { run(primary.intent) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .controlSize(.small)
        .frame(minHeight: Metric.popoverRowHeight)
    }
}

/// One server that needs something: what it is, what is wrong in a few words,
/// and its fix at the end of the row.
private struct TroubleRow: View {
    let server: ServerFacts
    let prominent: Bool
    let canFix: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.tight) {
            Button { run(.reveal(server: server.name)) } label: {
                HStack(spacing: Metric.snug) {
                    ServerGlyph(name: server.name, size: 24, status: server.health.color)
                    VStack(alignment: .leading, spacing: 0) {
                        Text(server.name)
                            .font(.callout)
                            .lineLimit(1)
                        Text(server.reason)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(server.name), \(server.reason)")
            .help(server.reason)

            fixControl
        }
        .frame(minHeight: Metric.popoverRowHeight + Metric.tight)
    }

    @ViewBuilder private var fixControl: some View {
        if let cancel = server.cancelSignIn {
            ProgressView().controlSize(.small)
            Button(cancel.title) { run(cancel.intent) }
                .controlSize(.small)
                .accessibilityLabel("\(cancel.title), \(server.name)")
        } else if let fix = server.fix {
            Group {
                if prominent {
                    Button(fix.title) { run(fix.intent) }.buttonStyle(.borderedProminent)
                } else {
                    Button(fix.title) { run(fix.intent) }
                }
            }
            .controlSize(.small)
            .disabled(!canFix)
            .accessibilityLabel("\(fix.title), \(server.name)")
        }
    }
}

/// The small picture at the start of a plain line in the panel. Each is drawn
/// here, in one weight, and fits both the words and the button beside it.
struct PanelIcon: View {
    enum Kind: Equatable, Sendable {
        /// A stethoscope, for a checkup.
        case checkup
        /// A plug with its cord loose, for Plug not running.
        case plug
        /// A server's tile with a plus, for adding one.
        case addServer
    }

    let kind: Kind

    var body: some View {
        Mark(kind: kind)
            .stroke(.secondary, style: StrokeStyle(lineWidth: 1.6, lineCap: .round, lineJoin: .round))
            .frame(width: 16, height: 16)
            .accessibilityHidden(true)
    }

    /// Drawn on a 16 by 16 grid.
    private struct Mark: Shape {
        let kind: Kind

        func path(in rect: CGRect) -> Path {
            var path = Path()
            switch kind {
            case .checkup:
                path.move(to: CGPoint(x: 3.5, y: 2.5))
                path.addLine(to: CGPoint(x: 3.5, y: 6.5))
                path.addRelativeArc(center: CGPoint(x: 6, y: 6.5), radius: 2.5, startAngle: .degrees(180), delta: .degrees(-180))
                path.addLine(to: CGPoint(x: 8.5, y: 2.5))
                path.move(to: CGPoint(x: 6, y: 9))
                path.addLine(to: CGPoint(x: 6, y: 10.5))
                path.addRelativeArc(center: CGPoint(x: 9.25, y: 10.5), radius: 3.25, startAngle: .degrees(180), delta: .degrees(-180))
                path.addLine(to: CGPoint(x: 12.5, y: 9.6))
                path.addEllipse(in: CGRect(x: 10.7, y: 5.9, width: 3.6, height: 3.6))
            case .plug:
                for x in [6.2, 9.8] {
                    path.move(to: CGPoint(x: x, y: 2.5))
                    path.addLine(to: CGPoint(x: x, y: 5.5))
                }
                path.move(to: CGPoint(x: 4.5, y: 5.5))
                path.addLine(to: CGPoint(x: 11.5, y: 5.5))
                path.addLine(to: CGPoint(x: 11.5, y: 7.5))
                path.addRelativeArc(center: CGPoint(x: 8, y: 7.5), radius: 3.5, startAngle: .degrees(0), delta: .degrees(180))
                path.closeSubpath()
                path.move(to: CGPoint(x: 8, y: 11))
                path.addLine(to: CGPoint(x: 8, y: 11.8))
                path.addCurve(
                    to: CGPoint(x: 4.8, y: 14),
                    control1: CGPoint(x: 8, y: 13.4), control2: CGPoint(x: 4.8, y: 12.2)
                )
            case .addServer:
                path.addRoundedRect(
                    in: CGRect(x: 2.5, y: 2.5, width: 11, height: 11), cornerSize: CGSize(width: 3.4, height: 3.4)
                )
                path.move(to: CGPoint(x: 8, y: 5.8))
                path.addLine(to: CGPoint(x: 8, y: 10.2))
                path.move(to: CGPoint(x: 5.8, y: 8))
                path.addLine(to: CGPoint(x: 10.2, y: 8))
            }
            return path.applying(CGAffineTransform(scaleX: rect.width / 16, y: rect.height / 16))
        }
    }
}

// MARK: - Lines

/// The last tool call: a dot for whether it worked, what ran, where, and how
/// long it took. Nil is a panel that has seen no calls yet.
private struct LatestCallLine: View {
    let call: CallFacts?

    var body: some View {
        HStack(spacing: Metric.tight + Metric.hairline) {
            Circle()
                .fill(dot)
                .frame(width: 7, height: 7)
                .frame(width: AppIconStack.size)
            if let call {
                Text(call.tool)
                    .font(.caption.monospaced())
                    .foregroundStyle(call.failed ? StatusColor.stopped : Color.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let server = call.server {
                    Text(server)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                Spacer(minLength: Metric.tight)
                Text(call.failed ? call.result : call.duration)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.tertiary)
                    .fixedSize()
            } else {
                Text("No activity yet")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spoken)
    }

    private var dot: Color {
        guard let call else { return StatusColor.quiet.opacity(0.5) }
        if call.failed { return StatusColor.stopped }
        return call.cancelled ? StatusColor.quiet : StatusColor.working
    }

    private var spoken: String {
        guard let call else { return "No activity yet" }
        return "\(call.tool)\(call.server.map { ", \($0)" } ?? ""), \(call.result), \(call.spokenDuration)"
    }
}

/// Up to three connected app icons, overlapping, so the row says who is
/// connected before the words do.
private struct AppIconStack: View {
    static let size: CGFloat = 20

    let clients: [ConnectedClient]

    var body: some View {
        HStack(spacing: -Metric.rowGap) {
            ForEach(Array(clients.prefix(3).enumerated()), id: \.offset) { _, client in
                AppGlyph(target: client.target, name: client.name, appPath: client.appPath, size: Self.size)
            }
        }
        .accessibilityHidden(true)
    }
}
