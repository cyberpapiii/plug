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
            Button(action: onSelect) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(tool.shortName)
                        .font(.callout.monospaced())
                        .foregroundStyle(tool.isOn ? .primary : .secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let summary = tool.summary, !summary.isEmpty {
                        Text(summary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Show details for \(tool.shortName)")
            trailing
        }
        .padding(.vertical, Metric.tight)
        .help(tool.summary ?? tool.name)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder private var trailing: some View {
        if isBusy {
            ProgressView()
                .controlSize(.mini)
                .frame(width: 28)
                .accessibilityLabel("Updating \(tool.shortName)")
        } else if !canManage {
            Text(tool.isOn ? "On" : "Off")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else if let pattern = tool.lockedByPattern {
            Label("Off by rule", systemImage: "lock.fill")
                .font(.caption)
                .foregroundStyle(.secondary)
                .help("The pattern \(pattern) covers this tool. Remove it to switch this tool back on.")
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
            .controlSize(.small)
            .accessibilityLabel(tool.shortName)
        }
    }
}
