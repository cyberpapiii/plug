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

/// Plug's top row, the same in the menu bar panel, the window and Settings:
/// the character with how Plug is on its face, one line that says it, and
/// the switch.
struct PlugHeadline: View {
    let model: AppModel
    let run: (PlugIntent) -> Void
    var characterSize: CGFloat = 40

    var body: some View {
        let verdict = model.verdict
        HStack(spacing: Metric.panelGap) {
            PlugCharacter(mood: verdict.mood, mark: verdict.tone == .blocked ? StatusColor.stopped : StatusColor.needsYou)
                .foregroundStyle(.tint)
                .frame(width: characterSize, height: characterSize)
            Text(verdict.title)
                .font(PanelType.title)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: Metric.tight)
            ServicePowerToggle(model: model, run: run).labelsHidden()
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
    /// The space between one group and the next in the menu bar panel.
    static let panelGap: CGFloat = 12
    /// A server's tile in the menu bar panel, and the gap between two.
    static let panelTile: CGFloat = 28
    static let panelTileGap: CGFloat = 8
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

// MARK: - Type

/// The three sizes of text in the menu bar panel. Nothing in it is smaller
/// than `small`.
enum PanelType {
    /// The one line Plug says.
    static let title = Font.system(size: 15, weight: .semibold)
    /// A name, or a line you can press.
    static let line = Font.system(size: 13)
    /// What explains the line above it, and counts.
    static let small = Font.system(size: 12)
    static let code = Font.system(size: 12, design: .monospaced)
}

// MARK: - Tone

/// The colours that say how something is. Every status colour in Plug is one
/// of these, and each means one thing. Blue is not a status: it is Plug
/// itself, and the one button to press.
enum StatusColor {
    static let working = Color.green
    /// Starting, or off.
    static let quiet = Color.secondary
    static let needsYou = Color.orange
    /// Stopped, or failed.
    static let stopped = Color.red
    /// How strong the wash is behind a card that reports trouble.
    static let wash = 0.11
}

extension Verdict.Tone {
    var color: Color {
        switch self {
        case .good: StatusColor.working
        case .quiet, .busy: StatusColor.quiet
        case .attention: StatusColor.needsYou
        case .blocked: StatusColor.stopped
        }
    }
}

extension ServerHealth {
    var color: Color {
        switch self {
        case .working: StatusColor.working
        case .starting, .off: StatusColor.quiet
        case .signInNeeded, .notLoaded: StatusColor.needsYou
        case .down, .unknown: StatusColor.stopped
        }
    }

    /// Motion means "wait, this is changing". A server that is simply up is
    /// not changing, and a whole column of pulsing dots read as unsettled.
    var pulses: Bool { isSettling }

    /// Shape, not just colour, carries the state.
    var icon: PlugIcon.Kind {
        switch self {
        case .working: .working
        case .starting: .starting
        case .signInNeeded: .signIn
        case .down, .unknown: .stopped
        case .notLoaded: .needsYou
        case .off: .off
        }
    }
}

// MARK: - Small parts

/// A tool's name, set the way code is in a message so it reads as a name and
/// not as a sentence. `dimmed` is for a tool that is off.
struct ToolName: View {
    let name: String
    var failed = false
    var dimmed = false

    init(_ name: String, failed: Bool = false, dimmed: Bool = false) {
        self.name = name
        self.failed = failed
        self.dimmed = dimmed
    }

    var body: some View {
        Text(name)
            .font(PanelType.code)
            .foregroundStyle(failed ? AnyShapeStyle(StatusColor.stopped) : AnyShapeStyle(dimmed ? .secondary : .primary))
            .lineLimit(1)
            .truncationMode(.middle)
            .padding(.horizontal, Metric.tight)
            .padding(.vertical, Metric.hairline)
            .background(Color.primary.opacity(0.08), in: RoundedRectangle(cornerRadius: 5, style: .continuous))
    }
}

/// A server's state as one glyph. Carries its own accessibility wording so the
/// meaning never lives in colour alone. The large one fills a detail header's
/// glyph slot.
struct StatusGlyph: View {
    let health: ServerHealth
    var large = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if health.pulses, !reduceMotion {
                mark.phaseAnimator([1.0, 0.45]) { mark, opacity in
                    mark.opacity(opacity)
                } animation: { _ in .easeInOut(duration: 1) }
            } else {
                mark
            }
        }
        .frame(width: large ? Metric.glyphSlot : 18, height: large ? Metric.glyphSlot : 18)
        .accessibilityLabel(health.label)
    }

    private var mark: some View {
        PlugIcon(health.icon, size: large ? 28 : 18)
            .foregroundStyle(health.color)
    }
}

/// A calm, readable section title shared by lists, inspectors, and history.
struct SectionLabel: View {
    let text: String
    var trailing: String?

    var body: some View {
        HStack(spacing: Metric.tight) {
            Text(text)
                .font(PanelType.small.weight(.semibold))
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if let trailing {
                Text(trailing)
                    .font(PanelType.small.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityAddTraits(.isHeader)
    }
}

extension Section where Parent == SectionLabel, Content: View, Footer == EmptyView {
    /// A group in a form, named in the one heading style.
    init(heading: String, @ViewBuilder content: () -> Content) {
        self.init(content: content, header: { SectionLabel(text: heading) })
    }
}

/// The headline in the window's banner: Plug's character in the colour of
/// what it says, the one sentence, and its buttons. The menu bar panel says
/// the same words in its own top row, so the app cannot contradict itself.
struct VerdictView: View {
    let verdict: Verdict
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.snug) {
            PlugCharacter(mood: verdict.mood)
                .foregroundStyle(verdict.tone.color)
                .frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(verdict.title)
                    .font(.callout.weight(.medium))
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail = verdict.detail {
                    Text(detail)
                        .font(PanelType.small)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: Metric.tight)
            HStack(spacing: Metric.tight) {
                if let secondary = verdict.secondary {
                    Button(secondary.title) { run(secondary.intent) }
                }
                if let primary = verdict.primary {
                    Button(primary.title) { run(primary.intent) }
                        .buttonStyle(.borderedProminent)
                }
            }
            .controlSize(.small)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(verdict.title). \(verdict.detail ?? "")")
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
