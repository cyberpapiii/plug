import SwiftUI

// MARK: - Metrics

/// One spacing scale for the whole app. Every gap in Plug is one of these.
enum Metric {
    static let hairline: CGFloat = 2
    static let rowGap: CGFloat = 4
    static let tight: CGFloat = 6
    static let snug: CGFloat = 10
    static let regular: CGFloat = 14
    static let roomy: CGFloat = 20
    static let corner: CGFloat = 10
    static let popoverWidth: CGFloat = 340
    static let popoverMaxListHeight: CGFloat = 268
    static let popoverRowHeight: CGFloat = 36
    /// Keep long management lists readable on wide displays without making
    /// rows feel pinned to the window edges.
    static let contentMaxWidth: CGFloat = 960
}

// MARK: - Tone

extension Verdict.Tone {
    var color: Color {
        switch self {
        case .good: .green
        case .busy: .secondary
        case .attention: .orange
        case .blocked: .red
        }
    }

    /// Only trouble earns a filled, coloured badge. Calm states stay quiet.
    var isLoud: Bool {
        switch self {
        case .good, .busy: false
        case .attention, .blocked: true
        }
    }
}

extension ServerHealth {
    var color: Color {
        switch self {
        case .working: .green
        case .starting: .secondary
        case .signInNeeded: .orange
        case .down, .unknown: .red
        case .off: .secondary
        }
    }

    /// Shape, not just colour, carries the state.
    var symbol: String {
        switch self {
        case .working: "circle.fill"
        case .starting: "circle.dotted"
        case .signInNeeded: "person.badge.key.fill"
        case .down: "exclamationmark.circle.fill"
        case .unknown: "questionmark.circle.fill"
        case .off: "circle.slash"
        }
    }
}

// MARK: - Small parts

/// A server's state as one glyph. Carries its own accessibility wording so the
/// meaning never lives in colour alone.
struct StatusGlyph: View {
    let health: ServerHealth
    var size: Font = .body
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var livePulse = false

    var body: some View {
        Group {
            if health == .working {
                ZStack {
                    Circle()
                        .fill(health.color.opacity(livePulse ? 0.16 : 0.07))
                        .frame(width: 16, height: 16)
                        .scaleEffect(livePulse ? 1 : 0.72)
                    Circle()
                        .fill(health.color)
                        .frame(width: 7, height: 7)
                }
                .animation(
                    reduceMotion
                        ? nil
                        : .easeInOut(duration: 1.35).repeatForever(autoreverses: true),
                    value: livePulse
                )
            } else {
                Image(systemName: health.symbol)
                    .font(size)
                    .foregroundStyle(health.color)
                    .symbolRenderingMode(.hierarchical)
            }
        }
        .frame(width: 18, height: 18)
        .accessibilityLabel(health.label)
        .onAppear {
            livePulse = health == .working && !reduceMotion
        }
        .onChange(of: health) { _, newHealth in
            livePulse = newHealth == .working && !reduceMotion
        }
        .onChange(of: reduceMotion) { _, isReduced in
            livePulse = health == .working && !isReduced
        }
    }
}

/// A calm, readable section title shared by lists, inspectors, and history.
struct SectionLabel: View {
    let text: String
    var trailing: String?

    var body: some View {
        HStack(spacing: Metric.tight) {
            Text(text)
                .font(.callout.weight(.semibold))
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if let trailing {
                Text(trailing)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }
        }
        .accessibilityAddTraits(.isHeader)
    }
}

/// The headline. Rendered large in the popover, compact in the window banner,
/// but always the same words, so the app cannot contradict itself.
struct VerdictView: View {
    let verdict: Verdict
    var compact = false
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(alignment: .center, spacing: Metric.snug) {
            icon
            VStack(alignment: .leading, spacing: 1) {
                Text(verdict.title)
                    .font(compact ? .callout.weight(.medium) : .headline)
                    .foregroundStyle(.primary)
                if let detail = verdict.detail {
                    Text(detail)
                        .font(compact ? .caption : .subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: Metric.tight)
            buttons
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(verdict.title). \(verdict.detail ?? "")")
    }

    @ViewBuilder private var icon: some View {
        if verdict.tone == .busy {
            ProgressView()
                .controlSize(.small)
                .frame(width: 22, height: 22)
        } else {
            Image(systemName: verdict.symbol)
                .font(compact ? .body : .title3)
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(verdict.tone.color)
                .frame(width: 22, height: 22)
                .accessibilityHidden(true)
        }
    }

    @ViewBuilder private var buttons: some View {
        HStack(spacing: Metric.tight) {
            if let secondary = verdict.secondary {
                Button(secondary.title) { run(secondary.intent) }
                    .buttonStyle(.link)
            }
            if let primary = verdict.primary {
                Button(primary.title) { run(primary.intent) }
                    .buttonStyle(.borderedProminent)
                    .controlSize(compact ? .small : .regular)
            }
        }
    }
}

/// A problem next to the button that fixes it.
struct AttentionRow: View {
    let item: AttentionItem
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.snug) {
            Image(systemName: item.symbol)
                .font(.callout)
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(.orange)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 0) {
                Text(item.title)
                    .font(.callout.weight(.medium))
                Text(item.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: Metric.tight)
            if item.isWorking {
                ProgressView().controlSize(.small)
            } else if let button = item.button {
                Button(button.title) { run(button.intent) }
                    .controlSize(.small)
            }
        }
        .padding(.vertical, Metric.tight)
        .padding(.horizontal, Metric.snug)
        .nativeGlassSurface(tint: .orange.opacity(0.12))
        .accessibilityElement(children: .combine)
    }
}

/// One server, read-only and quiet. The same row shape in the popover and the
/// window so the two surfaces never feel like different apps.
struct ServerRow: View {
    let server: ServerFacts
    var showsTrailingDetail = true

    var body: some View {
        HStack(spacing: Metric.snug) {
            StatusGlyph(health: server.health)
            Text(server.name)
                .font(.callout)
                .foregroundStyle(server.enabled ? .primary : .secondary)
                .lineLimit(1)
            Spacer(minLength: Metric.tight)
            if showsTrailingDetail {
                Text(trailingText)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(server.name), \(server.health.label)")
    }

    private var trailingText: String {
        switch server.health {
        case .working: server.toolCount == 1 ? "1 tool" : "\(server.toolCount) tools"
        default: server.health.label
        }
    }
}

/// A row that reads as one tappable line: label, value, chevron.
struct DisclosureRow<Trailing: View>: View {
    let symbol: String
    let title: String
    let detail: String?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: Metric.snug) {
            Image(systemName: symbol)
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 0) {
                Text(title).font(.callout)
                if let detail {
                    Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: Metric.tight)
            trailing
        }
        .contentShape(Rectangle())
    }
}

// MARK: - Buttons

/// A full-width, quiet row button — the popover's footer vocabulary.
struct QuietRowButtonStyle: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Metric.snug)
            .padding(.vertical, Metric.tight)
            .background(
                RoundedRectangle(cornerRadius: 7)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.12 : (hovering ? 0.07 : 0)))
            )
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
    }
}

extension View {
    /// Standard inset for popover content blocks.
    func popoverInset() -> some View {
        padding(.horizontal, Metric.regular)
    }

    /// Use the system's real Liquid Glass on macOS 26 while keeping the same
    /// readable material hierarchy on the app's macOS 14–15 floor.
    @ViewBuilder
    func nativeGlassSurface(tint: Color? = nil) -> some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            glassEffect(.regular.tint(tint), in: .rect(cornerRadius: Metric.corner, style: .continuous))
        } else {
            background(.regularMaterial, in: RoundedRectangle(cornerRadius: Metric.corner))
        }
#else
        background(.regularMaterial, in: RoundedRectangle(cornerRadius: Metric.corner))
#endif
    }

    /// Native glass controls belong on the small action cluster, not on every
    /// row. That keeps the hierarchy calm while making the controls unmistakable.
    @ViewBuilder
    func nativeGlassButton() -> some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            buttonStyle(.glass)
        } else {
            buttonStyle(.bordered)
        }
#else
        buttonStyle(.bordered)
#endif
    }

    /// Quiet inset content follows its container's corner geometry on macOS 26.
    @ViewBuilder
    func nativeInsetSurface(_ fill: AnyShapeStyle) -> some View {
#if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            background(fill, in: .rect(cornerRadius: Metric.corner, style: .continuous))
        } else {
            background(fill, in: RoundedRectangle(cornerRadius: Metric.corner))
        }
#else
        background(fill, in: RoundedRectangle(cornerRadius: Metric.corner))
#endif
    }
}
