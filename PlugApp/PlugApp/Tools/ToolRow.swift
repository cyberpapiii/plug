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
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(tool.shortName)
                    .font(.body)
                    .foregroundStyle(tool.isOn ? .primary : .secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let summary = tool.summary, !summary.isEmpty {
                    Text(summary)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: onSelect) {
                Label("Show Details", systemImage: "info.circle")
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help("Show Details")
            trailing
        }
        .accessibilityElement(children: .contain)
    }

    /// The switch stays in place while a change is under way, so the row does
    /// not shift.
    @ViewBuilder private var trailing: some View {
        if let pattern = tool.lockedByPattern {
            Label("Off by Rule", systemImage: "lock.fill")
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
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.mini)
            .disabled(isBusy || !canManage)
            .accessibilityLabel(tool.shortName)
        }
    }
}
