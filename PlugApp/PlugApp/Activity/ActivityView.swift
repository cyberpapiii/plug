import PlugIPC
import SwiftUI

/// What Plug has been doing. A log table answered "what fields exist"; this
/// answers "what happened, and is anything going wrong repeatedly?" — so
/// failures are countable and grouped by day, every row has a real time on
/// it, and the selected row says what happened and what to do about it.
struct ActivityView: View {
    let model: AppModel
    @Bindable var router: Router
    @Binding var search: String
    let run: (PlugIntent) -> Void
    @Environment(\.splitPane) private var pane

    enum Scope: String, CaseIterable, Identifiable {
        case everything = "All"
        case problems = "Problems"
        var id: Self { self }
    }

    /// Kept on the router, because the list column filters by it and the
    /// detail column holds its control.
    private var scope: Scope { router.activityScope }

    var body: some View {
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading activity")
            } else if model.initialDataUnavailable {
                UnavailablePage(verdict: model.verdict, run: run)
            } else if model.activities.isEmpty {
                EmptyPage(
                    title: "No Activity",
                    message: "Each time a client uses a tool, it shows here with the client, the server, the time, and whether it worked.",
                    symbol: AppSection.activity.symbol
                )
            } else if visible.isEmpty {
                if scope == .problems, search.trimmingCharacters(in: .whitespaces).isEmpty {
                    EmptyPage(
                        title: "No Problems",
                        message: "No recent call failed.",
                        symbol: "checkmark.circle"
                    )
                } else {
                    NoSearchResults(text: search)
                }
            } else {
                ListDetail {
                    List(selection: $router.selectedCall) {
                        ForEach(groups, id: \.title) { group in
                            ListGroupHeader(group.title)
                            ForEach(group.events) { event in
                                ActivityRow(call: model.call(event)).tag(event.sequence)
                            }
                            .listRowSeparator(.hidden)
                        }
                    }
                } detail: {
                    if let selected {
                        CallDetail(
                            call: model.call(selected),
                            canShowServer: serverNames.contains(selected.server ?? ""),
                            run: run
                        )
                        .id(selected.sequence)
                    } else {
                        NoSelection(item: "Call", symbol: AppSection.activity.symbol)
                    }
                }
            }
        }
        .navigationSubtitle(model.activitySummary ?? "")
        .toolbar {
            // The window draws a section once per column; the filter sits
            // beside the search field.
            if pane != .list {
                ToolbarItem(placement: .primaryAction) {
                    Picker("Show", selection: $router.activityScope) {
                        ForEach(Scope.allCases) { scope in
                            Text(scope.rawValue).tag(scope)
                        }
                    }
                    .pickerStyle(.segmented)
                    .disabled(model.activities.isEmpty)
                    .help("Show every call, or only the ones that failed")
                }
            }
        }
        .onChange(of: visible.first?.sequence, initial: true) { keepSelectionVisible() }
        .onChange(of: scope) { keepSelectionVisible() }
    }

    private var selected: ActivityEvent? {
        visible.first { $0.sequence == router.selectedCall }
    }

    /// The newest call is selected until the owner picks one, so the right
    /// side is never an empty placeholder.
    private func keepSelectionVisible() {
        if selected != nil { return }
        Task { @MainActor in
            await Task.yield()
            router.selectedCall = visible.first?.sequence
        }
    }

    private var serverNames: Set<String> { Set(model.situation.servers.map(\.name)) }

    private var visible: [ActivityEvent] {
        let query = search.trimmingCharacters(in: .whitespaces)
        return model.activities
            .filter { scope == .everything || CallFacts($0).failed }
            .filter { event in
                guard !query.isEmpty else { return true }
                return event.method.localizedCaseInsensitiveContains(query)
                    || (event.tool ?? "").localizedCaseInsensitiveContains(query)
                    || (event.server ?? "").localizedCaseInsensitiveContains(query)
                    || (event.clientLabel ?? "").localizedCaseInsensitiveContains(query)
                    || (event.clientType ?? "").localizedCaseInsensitiveContains(query)
            }
            .sorted { $0.sequence > $1.sequence }
    }

    private struct DayGroup {
        let title: String
        let events: [ActivityEvent]
    }

    /// Grouped by day so a long list stays legible without a date column.
    private var groups: [DayGroup] {
        let calendar = Calendar.current
        var order: [String] = []
        var buckets: [String: [ActivityEvent]] = [:]
        for event in visible {
            let date = Date(timeIntervalSince1970: Double(event.occurredAtMs) / 1_000)
            let title: String
            if calendar.isDateInToday(date) {
                title = "Today"
            } else if calendar.isDateInYesterday(date) {
                title = "Yesterday"
            } else {
                title = date.formatted(.dateTime.weekday(.wide).month().day())
            }
            if buckets[title] == nil {
                order.append(title)
                buckets[title] = []
            }
            buckets[title]?.append(event)
        }
        return order.map { DayGroup(title: $0, events: buckets[$0] ?? []) }
    }
}

private struct ActivityRow: View {
    let call: CallFacts

    var body: some View {
        HStack(spacing: Metric.tight) {
            // Who called what, as two pictures: the client's icon, then
            // the server's. A long list can be scanned without reading it.
            HStack(spacing: Metric.hairline) {
                AppGlyph(target: call.callerTarget, name: call.callerIconName)
                if let server = call.server {
                    Image(systemName: "chevron.compact.right")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                    ServerGlyph(name: server)
                }
            }
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(call.tool)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(context)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: Metric.tight)
            // A failure is marked beside the time, so it shows at a glance.
            // A call the client stopped is not a failure.
            if call.failed {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.red)
                    .symbolRenderingMode(.hierarchical)
            }
            Text(time)
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
                .layoutPriority(1)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(call.result). \(call.tool)\(call.server.map { ", \($0)" } ?? ""), \(call.caller), \(time), \(call.spokenDuration)"
        )
    }

    /// Which server, then who called.
    private var context: String {
        [call.server, call.callerAndPlace].compactMap { $0 }.joined(separator: " · ")
    }

    private var time: String {
        call.date.formatted(date: .omitted, time: .shortened)
    }
}

/// One call in full: what ran, who asked, when, how long, and the result. A
/// failure says why and what to do next.
private struct CallDetail: View {
    let call: CallFacts
    let canShowServer: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        DetailForm {
            Section {
                DetailHeader(title: call.tool, subtitle: call.result) {
                    glyph
                        .font(.title2)
                        .symbolRenderingMode(.hierarchical)
                        .accessibilityHidden(true)
                } controls: {
                    EmptyView()
                }
                if call.failed {
                    ProblemNote(
                        title: call.reason == nil ? "No reason was recorded" : "The server reported an error",
                        reason: call.reason,
                        advice: call.advice
                    )
                }
            } footer: {
                if call.cancelled, let advice = call.advice {
                    Text(advice)
                }
            }
            Section("Details") {
                LabeledContent("Client") {
                    HStack(spacing: Metric.tight) {
                        AppGlyph(target: call.callerTarget, name: call.callerIconName, size: 16)
                        Text(call.callerAndPlace).textSelection(.enabled)
                    }
                }
                if let server = call.server {
                    LabeledContent("Server") {
                        HStack(spacing: Metric.tight) {
                            ServerGlyph(name: server, size: 16)
                            Text(server).textSelection(.enabled)
                            if canShowServer, let name = call.event.server {
                                Button { run(.reveal(server: name)) } label: {
                                    Image(systemName: "arrow.right.circle")
                                }
                                .buttonStyle(.borderless)
                                .foregroundStyle(.secondary)
                                .help("Show Server")
                                .accessibilityLabel("Show Server")
                            }
                        }
                    }
                }
                LabeledContent("Time", value: call.date.formatted(date: .abbreviated, time: .standard))
                LabeledContent("Duration", value: call.duration)
                LabeledContent("Full Name") {
                    Text(fullName)
                        .font(.body.monospaced())
                        .textSelection(.enabled)
                }
            }
        }
    }

    /// The name as the client called it, server half included.
    private var fullName: String {
        if let tool = call.event.tool, !tool.isEmpty { return tool }
        return call.event.method
    }

    @ViewBuilder private var glyph: some View {
        if call.succeeded {
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        } else if call.cancelled {
            Image(systemName: "minus.circle.fill").foregroundStyle(.secondary)
        } else {
            Image(systemName: "xmark.circle.fill").foregroundStyle(.red)
        }
    }
}
