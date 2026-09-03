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
    @Binding var query: String
    let run: (PlugIntent) -> Void

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
        VStack(spacing: 0) {
            PageHeader(title: "Tools", detail: toolsSummary) {
                if model.toolCatalog.offCount > 0 {
                    Picker("Show", selection: $showsOffOnly) {
                        Text("All").tag(false)
                        Text("Off \(model.toolCatalog.offCount)").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 132)
                }
            }

            Group {
                if model.isLoadingInitialData {
                    LoadingPage(message: "Loading tools…")
                } else if model.initialDataUnavailable {
                    UnavailablePage(item: "Tools") { run(.reconnect) }
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
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                            ForEach(groups) { group in
                                Section {
                                    ForEach(group.tools) { tool in
                                        toolRow(tool)
                                    }
                                } header: {
                                    ToolGroupHeader(group: group)
                                        .padding(.horizontal, Metric.roomy)
                                        .padding(.vertical, Metric.snug)
                                        .frame(minHeight: 38)
                                        .background(.background)
                                }
                            }
                        }
                        .frame(maxWidth: Metric.contentMaxWidth)
                        .frame(maxWidth: .infinity)
                    }
                    .scrollBounceBehavior(.basedOnSize)
                }
            }
        }
        .inspector(isPresented: inspectorShown) {
            if let selected {
                ToolDetailView(
                    tool: selected,
                    catalog: model.toolCatalog,
                    canManage: model.canManageTools && model.canMutate,
                    isBusy: model.busyTools.contains(selected.name),
                    router: router,
                    run: run
                )
                .inspectorColumnWidth(min: 280, ideal: 320, max: 380)
            }
        }
        .onChange(of: query) { clearSelectionAfterListUpdate() }
        .onChange(of: showsOffOnly) { clearSelectionAfterListUpdate() }
        .onChange(of: model.toolCatalog.offCount) { _, count in
            if count == 0 { showsOffOnly = false }
        }
    }

    private var toolsSummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = model.toolCatalog.tools.count
        let summary = "\(count) \(count == 1 ? "tool" : "tools")"
        return model.dataIsStale ? "Last known · \(summary)" : summary
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

    private func toolRow(_ tool: ToolFacts) -> some View {
        ToolRow(
            tool: tool,
            canManage: model.canManageTools && model.canMutate,
            isBusy: model.busyTools.contains(tool.name),
            onSelect: { router.selectedTool = tool.name },
            run: run
        )
        .padding(.horizontal, Metric.snug)
        .frame(minHeight: tool.summary?.isEmpty == false ? 46 : 36)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(router.selectedTool == tool.name ? Color.accentColor.opacity(0.14) : Color.clear)
        )
        .hoverHighlight(cornerRadius: 7)
        .padding(.horizontal, Metric.snug)
        .contentShape(Rectangle())
        .accessibilityAction(named: "Show details") {
            router.selectedTool = tool.name
        }
    }

    /// NSTableView is still finishing its own update when search or filtering
    /// changes. Clearing selection on the next main-actor turn avoids a
    /// reentrant delegate mutation while keeping the inspector honest.
    private func clearSelectionAfterListUpdate() {
        Task { @MainActor in
            await Task.yield()
            router.selectedTool = nil
        }
    }
}

private struct ToolGroupHeader: View {
    let group: ToolGroup

    var body: some View {
        HStack(spacing: Metric.snug) {
            Image(systemName: group.isFullyOff ? "shippingbox" : "shippingbox.fill")
                .font(.callout)
                .foregroundStyle(group.isFullyOff ? .tertiary : .secondary)
                .accessibilityHidden(true)
            Text(group.server)
                .font(.callout.weight(.semibold))
                .foregroundStyle(.secondary)
            Spacer(minLength: Metric.tight)
            Text(summary)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
        }
        .accessibilityAddTraits(.isHeader)
    }

    private var summary: String {
        let total = group.tools.count
        guard group.onCount < total else { return total == 1 ? "1 tool" : "\(total) tools" }
        return "\(group.onCount) of \(total) on"
    }
}

/// One tool: what it does, and whether connected apps can see it.
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
            .controlSize(.mini)
            .accessibilityLabel(tool.shortName)
        }
    }
}
