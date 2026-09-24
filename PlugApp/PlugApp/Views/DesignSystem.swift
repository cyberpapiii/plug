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
    /// Rows in the window's lists. The default inset list adds its own air
    /// around every row; this keeps rows readable without a gap between them.
    static let listRowInsets = EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12)
    static let popoverWidth: CGFloat = 340
    static let popoverRowHeight: CGFloat = 34
    /// Whole rows shown before the list scrolls.
    static let popoverVisibleRows = 7
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

    /// The soft field behind the headline icon. Green stays a whisper so the
    /// healthy panel reads calm; trouble is allowed to be louder.
    var tint: Color {
        switch self {
        case .good: .green.opacity(0.14)
        case .busy: .secondary.opacity(0.12)
        case .attention: .orange.opacity(0.18)
        case .blocked: .red.opacity(0.18)
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

/// The headline. Rendered as a hero in the popover, compact in the window
/// banner, but always the same words, so the app cannot contradict itself.
struct VerdictView: View {
    enum Style { case hero, compact }

    let verdict: Verdict
    let style: Style
    let run: (PlugIntent) -> Void

    private var compact: Bool { style == .compact }

    var body: some View {
        HStack(alignment: .center, spacing: style == .hero ? Metric.regular : Metric.snug) {
            icon
            VStack(alignment: .leading, spacing: style == .hero ? 2 : 1) {
                Text(verdict.title)
                    .font(titleFont)
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
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

    private var titleFont: Font {
        switch style {
        case .hero: .title3.weight(.semibold)
        case .compact: .callout.weight(.medium)
        }
    }

    @ViewBuilder private var icon: some View {
        switch style {
        case .hero:
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(verdict.tone.tint)
                if verdict.tone == .busy {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: verdict.symbol)
                        .font(.title3.weight(.medium))
                        .symbolRenderingMode(.hierarchical)
                        .foregroundStyle(verdict.tone.color)
                        .contentTransition(.symbolEffect(.replace))
                }
            }
            .frame(width: 40, height: 40)
            .accessibilityHidden(true)
        case .compact:
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

/// A soft field that appears under the pointer, so rows in a plain scroll
/// view feel as alive as rows in a list.
private struct HoverHighlight: ViewModifier {
    let cornerRadius: CGFloat
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .background(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .fill(Color.primary.opacity(hovering ? 0.05 : 0))
            )
            .animation(.easeOut(duration: 0.12), value: hovering)
            .onHover { hovering = $0 }
    }
}

extension View {
    func hoverHighlight(cornerRadius: CGFloat = 7) -> some View {
        modifier(HoverHighlight(cornerRadius: cornerRadius))
    }

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
