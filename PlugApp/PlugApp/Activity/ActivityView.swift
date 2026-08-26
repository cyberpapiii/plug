import PlugIPC
import SwiftUI

/// What Plug has been doing. A log table answered "what fields exist"; this
/// answers "what happened, and is anything going wrong repeatedly?" — so
/// failures are countable and grouped by day, and every row has a real time on
/// it instead of a bare latency number.
struct ActivityView: View {
    let model: AppModel
    @State private var scope: Scope = .everything
    @State private var search = ""

    enum Scope: String, CaseIterable, Identifiable {
        case everything = "Everything"
        case problems = "Problems"
        var id: Self { self }
    }

    var body: some View {
        Group {
            if model.activities.isEmpty {
                EmptyPage(
                    title: "Nothing yet",
                    message: "Every tool call your AI apps make through Plug will show up here.",
                    symbol: "clock.arrow.circlepath"
                )
            } else if visible.isEmpty {
                EmptyPage(
                    title: scope == .problems ? "No problems" : "No matches",
                    message: scope == .problems
                        ? "Every recent call went through cleanly."
                        : "Nothing recent matches that search.",
                    symbol: scope == .problems ? "checkmark.circle" : "magnifyingglass"
                )
            } else {
                List {
                    ForEach(groups, id: \.title) { group in
                        Section(group.title) {
                            ForEach(group.events) { event in
                                ActivityRow(event: event)
                            }
                        }
                    }
                }
                .listStyle(.inset)
            }
        }
        .searchable(text: $search, placement: .toolbar, prompt: "Search calls")
        .toolbar {
            ToolbarItem {
                Picker("Show", selection: $scope) {
                    ForEach(Scope.allCases) { scope in
                        Text(scope == .problems ? problemsLabel : scope.rawValue).tag(scope)
                    }
                }
                .pickerStyle(.segmented)
                .frame(width: 200)
            }
        }
        .navigationTitle("Activity")
    }

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
                    || (event.server ?? "").localizedCaseInsensitiveContains(query)
                    || (event.client ?? "").localizedCaseInsensitiveContains(query)
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
    let event: ActivityEvent

    var body: some View {
        HStack(spacing: Metric.snug) {
            Image(systemName: succeeded ? "checkmark.circle" : "exclamationmark.triangle.fill")
                .font(.callout)
                .foregroundStyle(succeeded ? Color.secondary : .orange)
                .frame(width: 18)
                .accessibilityLabel(succeeded ? "Succeeded" : event.outcome.capitalized)
            VStack(alignment: .leading, spacing: 0) {
                Text(event.method).font(.body).lineLimit(1).truncationMode(.middle)
                Text(context).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: Metric.tight)
            VStack(alignment: .trailing, spacing: 0) {
                Text(time).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                Text(latency).font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, Metric.tight - 2)
        .accessibilityElement(children: .combine)
    }

    private var succeeded: Bool { event.outcome == "success" }

    private var context: String {
        let parts = [event.client, event.server].compactMap { $0 }.filter { !$0.isEmpty }
        if parts.isEmpty { return succeeded ? "Plug" : event.outcome.capitalized }
        let joined = parts.joined(separator: " → ")
        return succeeded ? joined : "\(joined) · \(event.outcome)"
    }

    private var time: String {
        Date(timeIntervalSince1970: Double(event.occurredAtMs) / 1_000)
            .formatted(date: .omitted, time: .shortened)
    }

    private var latency: String {
        event.latencyMs >= 1_000
            ? String(format: "%.1fs", Double(event.latencyMs) / 1_000)
            : "\(event.latencyMs) ms"
    }
}
