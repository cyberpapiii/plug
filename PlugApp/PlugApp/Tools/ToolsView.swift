import SwiftUI

/// What connected apps can actually do, and the switch for each one.
///
/// This is the question the app could not answer before: a server row said
/// "118 tools" and stopped there. Tools are listed under the server they come
/// from, searchable across every server at once, and each one can be switched
/// off without touching the server it belongs to.
struct ToolsView: View {
    let model: AppModel
    @Bindable var router: Router
    let run: (PlugIntent) -> Void

    @State private var query = ""
    @State private var showsOffOnly = false

    private var groups: [ToolGroup] {
        let matched = model.toolCatalog.groups(matching: query)
        guard showsOffOnly else { return matched }
        return matched.compactMap { group in
            let off = group.tools.filter { !$0.isOn }
            return off.isEmpty ? nil : ToolGroup(server: group.server, tools: off)
        }
    }

    var body: some View {
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading tools…")
            } else if model.toolCatalog.isEmpty {
                EmptyPage(
                    title: "No tools yet",
                    message: "Tools appear here once a server is running.",
                    symbol: "wrench.and.screwdriver"
                )
            } else if groups.isEmpty {
                EmptyPage(
                    title: emptyTitle,
                    message: emptyMessage,
                    symbol: query.trimmingCharacters(in: .whitespaces).isEmpty
                        ? "checkmark.circle"
                        : "magnifyingglass"
                )
            } else {
                List(selection: $router.selectedTool) {
                    ForEach(groups) { group in
                        Section {
                            ForEach(group.tools) { tool in
                                ToolRow(tool: tool, canManage: model.canManageTools, run: run)
                            }
                        } header: {
                            ToolGroupHeader(group: group)
                        }
                    }
                }
                .listStyle(.inset)
            }
        }
        .inspector(isPresented: inspectorShown) {
            if let selected {
                ToolDetailView(
                    tool: selected,
                    catalog: model.toolCatalog,
                    canManage: model.canManageTools,
                    run: run
                )
                .inspectorColumnWidth(min: 280, ideal: 320, max: 380)
            }
        }
        .searchable(text: $query, placement: .toolbar, prompt: "Search tools")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Picker("Show", selection: $showsOffOnly) {
                    Label("All \(model.toolCatalog.tools.count)", systemImage: "square.grid.2x2").tag(false)
                    Label("Off \(model.toolCatalog.offCount)", systemImage: "circle.slash").tag(true)
                }
                .pickerStyle(.segmented)
                .frame(width: 160)
            }
        }
    }

    private var emptyTitle: String {
        query.trimmingCharacters(in: .whitespaces).isEmpty && showsOffOnly
            ? "Every tool is on"
            : "Nothing matches"
    }

    private var emptyMessage: String {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty
            ? "No tools are switched off."
            : "No tool matches “\(trimmed)”."
    }

    /// Same rule as the server list: the inspector is open exactly when
    /// something is selected, so the two cannot disagree.
    private var inspectorShown: Binding<Bool> {
        Binding(
            get: { selected != nil },
            set: { shown in if !shown { router.selectedTool = nil } }
        )
    }

    private var selected: ToolFacts? {
        model.toolCatalog.tools.first { $0.name == router.selectedTool }
    }
}

private struct ToolGroupHeader: View {
    let group: ToolGroup

    var body: some View {
        HStack(spacing: Metric.tight) {
            Image(systemName: group.isFullyOff ? "shippingbox" : "shippingbox.fill")
                .foregroundStyle(group.isFullyOff ? .tertiary : .secondary)
                .accessibilityHidden(true)
            Text(group.server)
            Spacer(minLength: Metric.tight)
            Text(summary)
                .foregroundStyle(.tertiary)
                .monospacedDigit()
        }
    }

    private var summary: String {
        let total = group.tools.count
        guard group.onCount < total else { return "\(total) on" }
        return "\(group.onCount) of \(total) on"
    }
}

/// One tool: what it does, and whether connected apps can see it.
struct ToolRow: View {
    let tool: ToolFacts
    let canManage: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Metric.snug) {
            VStack(alignment: .leading, spacing: 1) {
                Text(tool.shortName)
                    .font(.callout.monospaced())
                    .foregroundStyle(tool.isOn ? .primary : .secondary)
                if let summary = tool.summary, !summary.isEmpty {
                    Text(summary)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                } else if let pattern = tool.lockedByPattern {
                    Text("Switched off by the pattern \(pattern)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: Metric.tight)
            trailing
        }
        .padding(.vertical, Metric.hairline)
        .help(tool.name)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(tool.shortName), \(tool.isOn ? "on" : "off")")
    }

    @ViewBuilder private var trailing: some View {
        if !canManage {
            Text(tool.isOn ? "On" : "Off")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else if let pattern = tool.lockedByPattern {
            Label("Off", systemImage: "lock.fill")
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
            .controlSize(.mini)
        }
    }
}
