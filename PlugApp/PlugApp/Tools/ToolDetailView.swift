import SwiftUI

/// One tool, in full. The list answers "what exists and is it on"; this answers
/// the questions the list cannot fit — which server it came from, what a client
/// actually calls it, and, when it is held off by a pattern, what else that
/// pattern takes with it.
struct ToolDetailView: View {
    let tool: ToolFacts
    let catalog: ToolCatalog
    let canManage: Bool
    @Bindable var router: Router
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
        HStack(alignment: .top, spacing: Metric.snug) {
            Image(systemName: tool.isOn ? "wrench.and.screwdriver.fill" : "wrench.and.screwdriver")
                .font(.title2)
                .foregroundStyle(tool.isOn ? .secondary : .tertiary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(tool.shortName)
                    .font(.title3.weight(.semibold).monospaced())
                    .textSelection(.enabled)
                Text(tool.server).font(.callout).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Button {
                router.selectedTool = nil
            } label: {
                Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Close details")
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
                Text("The pattern \(pattern) covers this tool. Remove the pattern to switch it back on.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if canManage {
                Toggle(
                    tool.isOn ? "Connected apps can call this tool" : "Hidden from connected apps",
                    isOn: Binding(
                        get: { tool.isOn },
                        set: { run(.setToolEnabled(tool.name, $0)) }
                    )
                )
                .toggleStyle(.switch)
                .controlSize(.small)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Metric.regular)
        .background(
            tool.isOn ? AnyShapeStyle(.quaternary.opacity(0.3)) : AnyShapeStyle(.orange.opacity(0.1)),
            in: RoundedRectangle(cornerRadius: Metric.corner)
        )
    }

    // MARK: - Details

    private var details: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            SectionLabel(text: "Details")
            detailRow("Server", tool.server, symbol: "shippingbox")
            detailRow("State", stateLabel, symbol: tool.isOn ? "checkmark.circle" : "circle.slash")
            // The merged name is what a connected app actually sends, so it is
            // the value worth copying out of this panel.
            VStack(alignment: .leading, spacing: 2) {
                Label("Full name", systemImage: "number")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Text(tool.name)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
        }
    }

    private var stateLabel: String {
        guard !tool.isOn else { return "On" }
        return tool.lockedByPattern == nil ? "Off" : "Off, by pattern"
    }

    private func detailRow(_ label: String, _ value: String, symbol: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Label(label, systemImage: symbol).font(.callout).foregroundStyle(.secondary)
            Spacer(minLength: Metric.regular)
            Text(value).font(.callout).textSelection(.enabled)
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
                    text: "Also covered",
                    trailing: siblings.count == 1 ? "1 tool" : "\(siblings.count) tools"
                )
                ForEach(siblings.prefix(8)) { sibling in
                    Text(sibling.shortName)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                if siblings.count > 8 {
                    Text("and \(siblings.count - 8) more")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
            }
        }
    }
}
