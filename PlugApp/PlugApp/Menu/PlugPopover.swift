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
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let latest = Self.recentCalls(model.activities, limit: 1).first
        PanelView(
            facts: PanelFacts(
                verdict: model.verdict,
                situation: model.situation,
                serviceEnabled: model.serviceEnabled,
                stale: model.dataIsStale,
                canFix: model.canMutate,
                events: (model.snapshot.events ?? []).map(EventFacts.init(_:)),
                call: latest.map(model.call),
                callID: latest?.sequence
            ),
            error: model.actionError,
            power: ServicePowerToggle(model: model, run: run).labelsHidden(),
            send: send,
            quit: { run(.quit) }
        )
        .onAppear { model.setWatching(true) }
        .onDisappear { model.setWatching(false) }
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

/// Everything the panel shows, as plain values, so it can be drawn and
/// checked without a running Plug.
struct PanelFacts: Equatable {
    var verdict: Verdict
    var situation: PlugSituation
    var serviceEnabled = true
    /// The rows are what Plug said last, not what it says now.
    var stale = false
    var canFix = true
    var events: [EventFacts] = []
    /// The last tool call, and what tells one call from the next.
    var call: CallFacts?
    var callID: UInt64?
}

struct PanelView<Power: View>: View {
    let facts: PanelFacts
    var error: ActionError?
    let power: Power
    let send: (PlugIntent) -> Void
    let quit: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var situation: PlugSituation { facts.situation }
    private var trouble: PanelTrouble? {
        facts.serviceEnabled ? PanelTrouble.trouble(for: situation, verdict: facts.verdict) : nil
    }

    /// Off, stopped, or with no servers, there is nothing under the card to
    /// show.
    private var showsBody: Bool {
        facts.serviceEnabled && situation.setup == .ready && situation.runtime != .stopped
            && situation.runtime != .off && !situation.activeServers.isEmpty
    }

    var body: some View {
        let trouble = trouble
        VStack(alignment: .leading, spacing: 0) {
            header(trouble)
            if let error {
                ProblemNote(error) { send(.dismissActionError) }
                    .padding(.horizontal, Metric.panelInset)
                    .padding(.bottom, Metric.snug)
            }
            if let trouble {
                TroubleCard(trouble: trouble, canFix: facts.canFix, run: send)
                    .padding(.horizontal, Metric.tight + Metric.hairline)
                    .padding(.bottom, Metric.panelGap)
                    .transition(.opacity)
            }
            if showsBody {
                VStack(alignment: .leading, spacing: Metric.snug) {
                    serversSection
                    clientsSection
                    eventsSection
                    activitySection
                }
                .opacity(facts.stale ? 0.55 : 1)
                .padding(.bottom, Metric.panelGap)
                .transition(.opacity)
            }
            Divider()
            footer
        }
        .frame(width: Metric.popoverWidth)
        .animation(reduceMotion ? nil : .snappy(duration: 0.25), value: facts.verdict)
        .animation(reduceMotion ? nil : .snappy(duration: 0.25), value: trouble)
        .animation(reduceMotion ? nil : .snappy(duration: 0.25), value: situation.settledServers)
        .animation(reduceMotion ? nil : .snappy(duration: 0.3), value: facts.callID)
    }

    // MARK: - Header

    /// The character is always Plug blue. Its face says how Plug is, and the
    /// small mark beside it takes the colour of the trouble.
    private func header(_ trouble: PanelTrouble?) -> some View {
        let verdict = facts.verdict
        return HStack(spacing: Metric.panelGap) {
            PlugCharacter(mood: verdict.mood, mark: trouble?.wash?.color ?? StatusColor.needsYou)
                .foregroundStyle(.tint)
                .frame(width: characterSize, height: characterSize)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(verdict.title)
                    .font(PanelType.title)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail = headerDetail {
                    Text(detail)
                        .font(PanelType.small)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            Spacer(minLength: Metric.tight)
            power
        }
        .padding(.horizontal, Metric.panelInset)
        .padding(.top, Metric.panelInset)
        .padding(.bottom, trouble == nil && !showsBody ? Metric.panelInset : Metric.regular)
    }

    private var characterSize: CGFloat { 44 }

    /// A second line only while Plug itself is on its way up. Trouble says
    /// more in its card, and all-is-well needs no more said.
    private var headerDetail: String? {
        let verdict = facts.verdict
        guard verdict.tone == .busy, situation.setup != .ready || situation.runtime != .running else { return nil }
        return verdict.detail
    }

    // MARK: - Servers

    /// Every server that needs nothing from you, as icons: the ones running,
    /// then the ones turned off, dimmed. Past two rows the rest are a number:
    /// the window lists them all.
    private var serversSection: some View {
        let servers = situation.settledServers + situation.offServers
        let shown = servers.count > shelfLimit ? Array(servers.prefix(shelfLimit - 1)) : servers
        return PanelSection(
            title: "Servers", detail: situation.runningSummary(stale: facts.stale),
            open: { send(.openWindow(.servers)) }
        ) {
            if !servers.isEmpty {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: Metric.panelTile, maximum: Metric.panelTile), spacing: Metric.panelTileGap)],
                    alignment: .leading,
                    spacing: Metric.panelTileGap
                ) {
                    ForEach(shown) { server in
                        Button { send(.reveal(server: server.name)) } label: {
                            ShelfTile(server: server)
                        }
                        .buttonStyle(.plain)
                        .help("\(server.name) · \(server.shelfNote)")
                        .accessibilityLabel("\(server.name), \(server.shelfNote)")
                        .transition(.scale(scale: 0.6).combined(with: .opacity))
                    }
                    if shown.count < servers.count {
                        Text("+\(servers.count - shown.count)")
                            .font(PanelType.small.weight(.semibold).monospacedDigit())
                            .foregroundStyle(.secondary)
                            .frame(width: Metric.panelTile, height: Metric.panelTile)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: Metric.smallCorner, style: .continuous))
                            .accessibilityLabel("\(servers.count - shown.count) more servers")
                    }
                }
            }
        }
    }

    /// Two rows of the panel's width.
    private var shelfLimit: Int { 16 }

    // MARK: - Clients

    private var clientsSection: some View {
        PanelSection(title: "Clients", open: { send(.openWindow(.clients)) }) {
            HStack(spacing: Metric.snug) {
                if !situation.connectedClients.isEmpty {
                    AppIconStack(clients: situation.connectedClients)
                }
                Text(situation.connectedSummary)
                    .font(PanelType.line)
                    .foregroundStyle(situation.connectedApps == 0 ? Color.secondary : Color.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(minHeight: AppIconStack.size)
        }
    }

    // MARK: - Events

    /// Shown only once there is an event to speak of.
    @ViewBuilder private var eventsSection: some View {
        let events = facts.events
        if !events.isEmpty {
            let failing = events.filter(\.health.needsAttention).count
            PanelSection(title: "Events", open: { send(.openWindow(.events)) }) {
                HStack(spacing: Metric.snug) {
                    Circle()
                        .fill(failing > 0 ? StatusColor.needsYou : StatusColor.working)
                        .frame(width: 8, height: 8)
                        .frame(width: AppIconStack.size)
                    Text(PlugSituation.eventsSummary(count: events.count, failing: failing))
                        .font(PanelType.line)
                        .lineLimit(1)
                }
                .frame(minHeight: AppIconStack.size)
            }
        }
    }

    // MARK: - Activity

    /// The last tool call, on one line. A new one rolls in from below.
    private var activitySection: some View {
        PanelSection(title: "Activity", open: { send(.openWindow(.activity)) }) {
            Group {
                if let call = facts.call {
                    LatestCallLine(call: call)
                        .id(facts.callID)
                        .transition(.push(from: .bottom))
                } else {
                    LatestCallLine(call: nil)
                }
            }
            .frame(maxWidth: .infinity, minHeight: AppIconStack.size, alignment: .leading)
            .clipped()
        }
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
            Button(action: quit) {
                Label("Quit Plug", systemImage: "power")
            }
            .labelStyle(.iconOnly)
            .keyboardShortcut("q", modifiers: .command)
            .help("Quit Plug")
        }
        .buttonStyle(.accessoryBar)
        .font(PanelType.line)
        .padding(Metric.tight + Metric.hairline)
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
        .padding(.leading, Metric.tight + Metric.hairline)
        .padding(.trailing, Metric.snug)
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
                    .frame(width: Metric.panelTile, height: Metric.panelTile)
            }
            if let note = trouble.note {
                Text(note)
                    .font(PanelType.small)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Spacer(minLength: Metric.tight)
            }
            if let secondary = trouble.secondary {
                Button(secondary.title) { run(secondary.intent) }
            }
            if let primary = trouble.primary {
                Button(primary.title) { run(primary.intent) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .frame(minHeight: TroubleRow.height)
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
                    ServerGlyph(name: server.name, size: Metric.panelTile, status: server.health.color)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(server.name)
                            .font(PanelType.line.weight(.medium))
                            .lineLimit(1)
                        Text(server.reason)
                            .font(PanelType.small)
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
        .frame(minHeight: Self.height)
    }

    static let height: CGFloat = 44

    @ViewBuilder private var fixControl: some View {
        if let cancel = server.cancelSignIn {
            ProgressView().controlSize(.small)
            Button(cancel.title) { run(cancel.intent) }
                .accessibilityLabel("\(cancel.title), \(server.name)")
        } else if let fix = server.fix {
            Group {
                if prominent {
                    Button(fix.title) { run(fix.intent) }.buttonStyle(.borderedProminent)
                } else {
                    Button(fix.title) { run(fix.intent) }
                }
            }
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
            .frame(width: 18, height: 18)
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

// MARK: - Sections

/// One group in the panel: its name, which opens its page in the window, and
/// what it has to show.
private struct PanelSection<Content: View>: View {
    let title: String
    var detail: String?
    let open: () -> Void
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.hairline) {
            Button(action: open) {
                HStack(spacing: Metric.rowGap) {
                    Text(title)
                        .fontWeight(.semibold)
                    Spacer(minLength: Metric.tight)
                    if let detail {
                        Text(detail)
                            .lineLimit(1)
                            .contentTransition(.numericText())
                    }
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.tertiary)
                }
                .font(PanelType.small)
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
            }
            .buttonStyle(QuietRowButtonStyle())
            .padding(.horizontal, Metric.tight)
            .help("Show \(title)")
            content
                .padding(.horizontal, Metric.panelInset)
        }
    }
}

// MARK: - Lines

/// One server on the shelf. Off is the same tile with the colour and the dot
/// taken away.
private struct ShelfTile: View {
    let server: ServerFacts

    var body: some View {
        ServerGlyph(
            name: server.name, size: Metric.panelTile, status: server.enabled ? server.health.color : nil
        )
        .saturation(server.enabled ? 1 : 0)
        .opacity(server.enabled ? (server.health == .working ? 1 : 0.5) : 0.45)
    }
}

extension ServerFacts {
    /// What the shelf says about a server when you point at it.
    var shelfNote: String {
        guard enabled else { return "Off" }
        return health == .working ? toolCountText : health.label
    }
}

/// The last tool call: a dot for whether it worked, what ran, where, and how
/// long it took. Nil is a panel that has seen no calls yet.
private struct LatestCallLine: View {
    let call: CallFacts?

    var body: some View {
        HStack(spacing: Metric.snug) {
            Circle()
                .fill(dot)
                .frame(width: 8, height: 8)
                .frame(width: AppIconStack.size)
            if let call {
                Text(call.tool)
                    .font(PanelType.code)
                    .foregroundStyle(call.failed ? StatusColor.stopped : Color.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let server = call.server {
                    Text(server)
                        .font(PanelType.small)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                Spacer(minLength: Metric.tight)
                Text(call.failed ? call.result : call.duration)
                    .font(PanelType.small.monospacedDigit())
                    .foregroundStyle(.tertiary)
                    .fixedSize()
            } else {
                Text("No activity yet")
                    .font(PanelType.line)
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
    static let size: CGFloat = 22

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
