import SwiftUI

/// One tool, in full. The list answers "what exists and is it on"; this answers
/// the questions the list cannot fit — which server it came from, what a client
/// actually calls it, and, when it is held off by a rule, what else that
/// rule takes with it.
struct ToolDetailView: View {
    let tool: ToolFacts
    let catalog: ToolCatalog
    let canManage: Bool
    let isBusy: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Metric.roomy) {
                header
                state
                details
                if tool.lockedByPattern != nil { covered }
            }
            .padding(Metric.roomy)
        }
        .scrollBounceBehavior(.basedOnSize)
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Metric.hairline) {
            Text(tool.shortName)
                .font(.headline)
                .textSelection(.enabled)
            Text(tool.server).font(.callout).foregroundStyle(.secondary)
        }
    }

    // MARK: - State

    @ViewBuilder private var state: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            if let summary = tool.summary, !summary.isEmpty {
                Text(summary)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let pattern = tool.lockedByPattern {
                Text("A rule in the settings file (\(pattern)) keeps this tool off. Remove the rule to turn it back on.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                // The switch stays in place while a change is under way.
                HStack(spacing: Metric.tight) {
                    Toggle(
                        "Available to clients",
                        isOn: Binding(
                            get: { tool.isOn },
                            set: { run(.setToolEnabled(tool.name, $0)) }
                        )
                    )
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .disabled(isBusy || !canManage)
                    Spacer(minLength: 0)
                    if isBusy {
                        Text("Updating…")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Metric.regular)
        .nativeInsetSurface(.insetFill)
    }

    // MARK: - Details

    private var details: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            SectionLabel(text: "Details")
            LabeledContent {
                Text(tool.server).font(.callout).textSelection(.enabled)
            } label: {
                Text("Server")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            // The merged name is what a client actually sends, so it is the
            // value worth copying out of this panel.
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Full Name")
                    .font(PanelType.small)
                    .foregroundStyle(.secondary)
                Text(tool.name)
                    .font(.callout.monospaced())
                    .textSelection(.enabled)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
        }
    }

    // MARK: - Covered tools

    private var siblings: [ToolFacts] {
        guard let pattern = tool.lockedByPattern else { return [] }
        return catalog.tools(coveredBy: pattern).filter { $0.name != tool.name }
    }

    @ViewBuilder private var covered: some View {
        if !siblings.isEmpty {
            VStack(alignment: .leading, spacing: Metric.tight) {
                SectionLabel(
                    text: "Also Off by This Rule",
                    trailing: siblings.count == 1 ? "1 tool" : "\(siblings.count) tools"
                )
                ForEach(siblings.prefix(8)) { sibling in
                    Text(sibling.shortName)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                if siblings.count > 8 {
                    Text("and \(siblings.count - 8) more")
                        .font(PanelType.small)
                        .foregroundStyle(.tertiary)
                }
            }
        }
    }
}
