import PlugIPC
import SwiftUI

/// What Plug has been doing. A log table answered "what fields exist"; this
/// answers "what happened, and is anything going wrong repeatedly?" — so
/// failures are countable and grouped by day, and every row has a real time on
/// it instead of a bare latency number.
struct ActivityView: View {
    let model: AppModel
    @Binding var search: String
    let run: (PlugIntent) -> Void
    @State private var scope: Scope = .everything

    enum Scope: String, CaseIterable, Identifiable {
        case everything = "Everything"
        case problems = "Problems"
        var id: Self { self }
    }

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "Activity", detail: activitySummary) {
                if !model.activities.isEmpty {
                    Picker("Show", selection: $scope) {
                        ForEach(Scope.allCases) { scope in
                            Text(scope == .problems ? problemsLabel : scope.rawValue).tag(scope)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 190)
                }
            }

            Group {
                if model.isLoadingInitialData {
                    LoadingPage(message: "Loading activity…")
                } else if model.initialDataUnavailable {
                    UnavailablePage(item: "Activity") { run(.reconnect) }
                } else if model.activities.isEmpty {
                    EmptyPage(
                        title: "No activity yet",
                        message: "Tool calls will appear here with their app, server, time, and result.",
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
                            SectionLabel(
                                text: group.title,
                                trailing: group.events.count == 1 ? "1 call" : "\(group.events.count) calls"
                            )
                                .padding(.top, Metric.regular)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                            ForEach(group.events) { event in
                                ActivityRow(event: event)
                                    .listRowSeparator(.hidden)
                            }
                        }
                        if model.activityIsCapped {
                            Text("This is the most recent \(AppModel.activityLimit) calls. Older ones are not kept.")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                                .frame(maxWidth: .infinity, alignment: .center)
                                .listRowSeparator(.hidden)
                        }
                    }
                    .listStyle(.inset)
                    .frame(maxWidth: Metric.contentMaxWidth)
                    .frame(maxWidth: .infinity)
                }
            }
        }
    }

    private var activitySummary: String? {
        guard model.hasLoadedSnapshot else { return nil }
        let count = model.activities.count
        let summary = "\(count) recent \(count == 1 ? "call" : "calls")"
        return model.dataIsStale ? "Last known · \(summary)" : summary
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
    let event: ActivityEvent

    var body: some View {
        HStack(spacing: Metric.snug) {
            // The calling app's own icon, so a long list can be scanned by
            // picture rather than read line by line.
            AppGlyph(
                target: AppIcons.target(forClientType: event.clientType ?? ""),
                name: appName ?? "",
                size: 20
            )
            Image(systemName: succeeded ? "checkmark.circle" : "exclamationmark.triangle.fill")
                .font(.callout)
                .foregroundStyle(succeeded ? Color.secondary : .orange)
                .frame(width: 18)
                .accessibilityLabel(succeeded ? "Succeeded" : event.outcome.capitalized)
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                Text(headline).font(.body.monospaced()).lineLimit(1).truncationMode(.middle)
                Text(context).font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
            }
            Spacer(minLength: Metric.tight)
            VStack(alignment: .trailing, spacing: Metric.rowGap) {
                Text(time).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                Text(latency).font(.caption2.monospacedDigit()).foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, Metric.snug)
        .accessibilityElement(children: .combine)
    }

    private var succeeded: Bool { event.outcome == "success" }

    /// What was called. The tool name is the useful part; `tools/call` is not.
    private var headline: String {
        guard let tool = event.tool, !tool.isEmpty else { return event.method }
        return tool
    }

    /// Who called it. Names the app, and separately the window or session
    /// inside that app, because two Claude Code windows are two callers.
    private var context: String {
        var parts: [String] = []
        if let app = appName { parts.append(app) }
        if let session = sessionTag { parts.append(session) }
        if let server = event.server, !server.isEmpty { parts.append(server) }
        if parts.isEmpty { parts.append("Plug") }
        let joined = parts.joined(separator: " · ")
        return succeeded ? joined : "\(joined) · \(event.outcome)"
    }

    private var appName: String? {
        if let label = event.clientLabel, !label.isEmpty { return label }
        guard let type = event.clientType, !type.isEmpty, type.lowercased() != "unknown" else {
            return nil
        }
        return type
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
    }

    /// A short, stable stand-in for one connection of that app. The full value
    /// is a UUID nobody can read; the leading characters are enough to tell two
    /// open windows apart, which is the only thing it is for.
    private var sessionTag: String? {
        guard let client = event.client, !client.isEmpty else { return nil }
        return "session \(client.prefix(4))"
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
