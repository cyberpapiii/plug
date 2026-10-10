import SwiftUI

/// One control and confirmation for the menu panel and service settings.
struct ServicePowerToggle: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    @State private var confirmingOff = false

    var body: some View {
        Toggle("Plug", isOn: Binding(
            get: { model.serviceEnabled },
            set: { enabled in
                if enabled { run(.setServiceEnabled(true)) }
                else { confirmingOff = true }
            }
        ))
        .toggleStyle(.switch)
        .disabled(model.isChangingService || model.isRestartingService)
        .accessibilityLabel("Plug")
        .help(model.serviceEnabled ? "Turn Plug off" : "Turn Plug on")
        .confirmationDialog("Turn off Plug?", isPresented: $confirmingOff, titleVisibility: .visible) {
            Button("Turn Off", role: .destructive) { run(.setServiceEnabled(false)) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Connected clients lose access to every server until you turn Plug on again. Your settings stay saved.")
        }
    }
}

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
    /// The hover field behind a quiet row.
    static let smallCorner: CGFloat = 7
    /// The one left edge every line of text in the menu bar panel shares.
    static let panelInset: CGFloat = tight + snug
    /// The picture at the start of a detail header.
    static let glyphSlot: CGFloat = 32
    static let popoverWidth: CGFloat = 340
    static let popoverRowHeight: CGFloat = 34
    /// Whole rows shown before the list scrolls.
    static let popoverVisibleRows = 7
    /// The list beside a detail, the same width in every section.
    static let listWidth: CGFloat = 270
    /// Room for the window buttons, the sidebar button, and a title with its count.
    static let listWidthUnderTitle: CGFloat = 350
    /// A detail reads best as a column, not stretched across a wide window.
    static let detailMaxWidth: CGFloat = 680
    static let settingsWidth: CGFloat = 520
    static let settingsHeight: CGFloat = 520
    static let sheetWidth: CGFloat = 520
}

// MARK: - Tone

extension Verdict.Tone {
    var color: Color {
        switch self {
        case .good: .green
        case .quiet: .secondary
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
        case .quiet: .secondary.opacity(0.12)
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
        case .signInNeeded, .notLoaded: .orange
        case .down, .unknown: .red
        case .off: .secondary
        }
    }

    /// Motion means "wait, this is changing". A server that is simply up is
    /// not changing, and a whole column of pulsing dots read as unsettled.
    var pulses: Bool { isSettling }

    /// Shape, not just colour, carries the state.
    var symbol: String {
        switch self {
        case .working: "circle.fill"
        case .starting: "circle.dotted"
        case .signInNeeded: "person.badge.key.fill"
        case .down, .unknown: "xmark.circle.fill"
        case .notLoaded: "exclamationmark.triangle.fill"
        case .off: "circle.slash"
        }
    }
}

// MARK: - Small parts

/// A server's state as one glyph. Carries its own accessibility wording so the
/// meaning never lives in colour alone. The large one fills a detail header's
/// glyph slot.
struct StatusGlyph: View {
    let health: ServerHealth
    var large = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if health == .working {
                ZStack {
                    Circle()
                        .fill(health.color.opacity(0.12))
                        .frame(width: large ? 28 : 14, height: large ? 28 : 14)
                    Circle()
                        .fill(health.color)
                        .frame(width: large ? 12 : 7, height: large ? 12 : 7)
                }
            } else {
                Image(systemName: health.symbol)
                    .font(large ? .title2 : .body)
                    .foregroundStyle(health.color)
                    .symbolRenderingMode(.hierarchical)
                    .symbolEffect(.pulse, options: .repeating, isActive: health.pulses && !reduceMotion)
            }
        }
        .frame(width: large ? Metric.glyphSlot : 18, height: large ? Metric.glyphSlot : 18)
        .accessibilityLabel(health.label)
    }
}

/// A calm, readable section title shared by lists, inspectors, and history.
struct SectionLabel: View {
    let text: String
    var trailing: String?

    var body: some View {
        HStack(spacing: Metric.tight) {
            Text(text)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if let trailing {
                Text(trailing)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
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
        Group {
            switch style {
            case .hero:
                // The buttons get their own line, so a long title is not
                // squeezed in the narrow panel.
                VStack(alignment: .leading, spacing: Metric.snug) {
                    HStack(spacing: Metric.regular) {
                        icon
                        textColumn
                        Spacer(minLength: 0)
                    }
                    if verdict.primary != nil || verdict.secondary != nil {
                        buttons.padding(.leading, Self.heroIconSize + Metric.regular)
                    }
                }
            case .compact:
                HStack(spacing: Metric.snug) {
                    icon
                    textColumn
                    Spacer(minLength: Metric.tight)
                    buttons
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(verdict.title). \(verdict.detail ?? "")")
    }

    private static let heroIconSize: CGFloat = 40

    private var textColumn: some View {
        VStack(alignment: .leading, spacing: Metric.hairline) {
            Text(verdict.title)
                .font(titleFont)
                .foregroundStyle(.primary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            if let detail = verdict.detail {
                Text(detail)
                    .font(compact ? .caption : .subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(compact ? 2 : 3)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
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
                RoundedRectangle(cornerRadius: Metric.corner, style: .continuous)
                    .fill(verdict.tone.tint)
                PlugCharacter(mood: PlugCharacter.Mood(verdict.tone))
                    .foregroundStyle(verdict.tone.color)
                    .padding(Metric.tight)
            }
            .frame(width: Self.heroIconSize, height: Self.heroIconSize)
            .accessibilityHidden(true)
        case .compact:
            PlugCharacter(mood: PlugCharacter.Mood(verdict.tone))
                .foregroundStyle(verdict.tone.color)
                .frame(width: 22, height: 22)
        }
    }

    @ViewBuilder private var buttons: some View {
        HStack(spacing: Metric.tight) {
            if let secondary = verdict.secondary {
                Button(secondary.title) { run(secondary.intent) }
            }
            if let primary = verdict.primary {
                Button(primary.title) { run(primary.intent) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .controlSize(compact ? .small : .regular)
    }
}

// MARK: - Fills

extension ShapeStyle where Self == AnyShapeStyle {
    /// The quiet field behind inset content in sheets and the guide.
    static var insetFill: AnyShapeStyle {
        AnyShapeStyle(HierarchicalShapeStyle.quaternary.opacity(0.3))
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
                RoundedRectangle(cornerRadius: Metric.smallCorner, style: .continuous)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.12 : (hovering ? 0.07 : 0)))
            )
            .contentShape(RoundedRectangle(cornerRadius: Metric.smallCorner, style: .continuous))
            .onHover { hovering = $0 }
    }
}

extension View {
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
