import SwiftUI

/// One tool: what it does, and whether connected clients can see it.
struct ToolRow: View {
    let tool: ToolFacts
    let canManage: Bool
    let isBusy: Bool
    let onSelect: () -> Void
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(spacing: Metric.snug) {
            // The words are the way in to a tool's details.
            Button(action: onSelect) {
                VStack(alignment: .leading, spacing: Metric.rowGap) {
                    ToolName(tool.shortName, dimmed: !tool.isOn)
                    if let summary = tool.summary, !summary.isEmpty {
                        Text(summary)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Show Details")
            trailing
        }
        .accessibilityElement(children: .contain)
    }

    /// The switch stays in place while a change is under way, so the row does
    /// not shift.
    @ViewBuilder private var trailing: some View {
        if let pattern = tool.lockedByPattern {
            Label("Off by Rule", icon: .locked)
                .font(.callout)
                .foregroundStyle(.secondary)
                .help("A rule in the settings file (\(pattern)) keeps this tool off. Remove the rule to turn it back on.")
        } else {
            Toggle(
                "On",
                isOn: Binding(
                    get: { tool.isOn },
                    set: { run(.setToolEnabled(tool.name, $0)) }
                )
            )
            .toggleStyle(RowSwitchStyle())
            .disabled(isBusy || !canManage)
            .accessibilityLabel(tool.shortName)
        }
    }
}

/// The small switch on a tool's row, drawn here. The system's switch is a
/// whole AppKit control: a long list of them made a server's page slow to open
/// and to scroll. This one is the same size and moves the same way.
private struct RowSwitchStyle: ToggleStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        Button {
            configuration.isOn.toggle()
        } label: {
            Capsule()
                .fill(configuration.isOn ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.quaternary))
                .frame(width: 36, height: 16)
                .overlay(alignment: configuration.isOn ? .trailing : .leading) {
                    Capsule()
                        .fill(.white.opacity(0.9))
                        .frame(width: 21, height: 13)
                        .padding(.horizontal, 1.5)
                }
                .animation(.snappy(duration: 0.2), value: configuration.isOn)
                .opacity(isEnabled ? 1 : 0.5)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(.isToggle)
        .accessibilityValue(configuration.isOn ? "On" : "Off")
    }
}
