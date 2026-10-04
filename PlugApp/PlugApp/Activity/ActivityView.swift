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
    @State private var scope: Scope = .everything

    enum Scope: String, CaseIterable, Identifiable {
        case everything = "Everything"
        case problems = "Problems"
        var id: Self { self }
    }

    var body: some View {
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading activity…")
            } else if model.initialDataUnavailable {
                UnavailablePage(item: "Activity") { run(.reconnect) }
            } else if model.activities.isEmpty {
                EmptyPage(
                    title: "No activity yet",
                    message: "Each time a client uses a tool, it shows here with the client, the server, the time, and whether it worked.",
                    symbol: "clock"
                )
            } else if visible.isEmpty {
                if scope == .problems, search.trimmingCharacters(in: .whitespaces).isEmpty {
                    EmptyPage(
                        title: "No problems",
                        message: "Every recent call went through cleanly.",
                        symbol: "checkmark.circle"
                    )
                } else {
                    ContentUnavailableView.search(text: search)
                }
            } else {
                ListDetail {
                    List(selection: $router.selectedCall) {
                        ForEach(groups, id: \.title) { group in
                            Section(group.title) {
                                ForEach(group.events) { event in
                                    ActivityRow(call: CallFacts(event)).tag(event.sequence)
                                }
                            }
                        }
                        if model.activityIsCapped {
                            Text("This is the most recent \(AppModel.activityLimit) calls. Older ones are not kept.")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                                .listRowSeparator(.hidden)
                        }
                    }
                } detail: {
                    if let selected {
                        CallDetail(
                            call: CallFacts(selected),
                            canShowServer: serverNames.contains(selected.server ?? ""),
                            run: run
                        )
                        .id(selected.sequence)
                    } else {
                        NoSelection(item: "Call")
                    }
                }
            }
        }
        .navigationSubtitle(activitySummary ?? "")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Picker("Show", selection: $scope) {
                    ForEach(Scope.allCases) { scope in
                        Text(scope == .problems ? problemsLabel : scope.rawValue).tag(scope)
                    }
                }
                .pickerStyle(.segmented)
                .disabled(model.activities.isEmpty)
                .help("Show every call, or only the ones that went wrong")
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

    private var activitySummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = model.activities.count
        guard count > 0 else { return nil }
        let summary = "\(count) recent \(count == 1 ? "call" : "calls")"
        return model.dataIsStale ? "Last known · \(summary)" : summary
    }

    private var serverNames: Set<String> { Set(model.situation.servers.map(\.name)) }

    private var problemsLabel: String {
        let count = model.activities.filter { $0.outcome != "success" }.count
        return count == 0 ? "Problems" : "Problems (\(count))"
    }

    private var visible: [ActivityEvent] {
        let query = search.trimmingCharacters(in: .whitespaces)
        return model.activities
            .filter { scope == .everything || $0.outcome != "success" }
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
            // The calling client's own icon, so a long list can be scanned
            // by picture rather than read line by line. Trouble replaces
            // the icon with a warning, so a failure is visible at a glance.
            Group {
                if call.succeeded {
                    AppGlyph(target: call.callerTarget, name: call.caller, size: 20)
                } else {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .symbolRenderingMode(.hierarchical)
                }
            }
            .frame(width: 20, height: 20)
            VStack(alignment: .leading, spacing: 1) {
                Text(call.tool)
                    .font(.callout.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(context)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: Metric.tight)
            Text(time)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .layoutPriority(1)
        }
        .padding(.vertical, Metric.hairline)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(call.result). \(call.tool)\(call.server.map { ", \($0)" } ?? ""), \(call.caller), \(time), \(call.spokenDuration)"
        )
    }

    /// Which server, then who called.
    private var context: String {
        [call.server, call.caller].compactMap { $0 }.joined(separator: " · ")
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
                DetailHeader(title: call.tool, subtitle: call.result, monospaced: true) {
                    Image(systemName: call.succeeded ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .font(.title2)
                        .symbolRenderingMode(.hierarchical)
                        .foregroundStyle(call.succeeded ? Color.green : .orange)
                        .accessibilityHidden(true)
                } controls: {
                    EmptyView()
                }
                if !call.succeeded {
                    ProblemNote(
                        title: call.reason ?? "This call \(call.result.lowercased()).",
                        advice: call.advice
                    )
                }
            }
            Section {
                if let server = call.server {
                    LabeledContent("Server") {
                        HStack(spacing: Metric.tight) {
                            Text(server).textSelection(.enabled)
                            if canShowServer, let name = call.event.server {
                                Button { run(.reveal(server: name)) } label: {
                                    Image(systemName: "arrow.right.circle.fill")
                                }
                                .buttonStyle(.borderless)
                                .help("Show this server")
                                .accessibilityLabel("Show Server")
                            }
                        }
                    }
                }
                LabeledContent("Client", value: call.caller)
                LabeledContent("When", value: call.date.formatted(date: .abbreviated, time: .standard))
                LabeledContent("Took", value: call.duration)
            }
        }
    }
}
