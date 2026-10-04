import PlugIPC
import SwiftUI

/// What Plug has been doing. A log table answered "what fields exist"; this
/// answers "what happened, and is anything going wrong repeatedly?" — so
/// failures are countable and grouped by day, every row has a real time on
/// it, and every row opens to say what happened and what to do about it.
struct ActivityView: View {
    let model: AppModel
    @Binding var search: String
    let run: (PlugIntent) -> Void
    @State private var scope: Scope = .everything
    /// The call whose detail is open.
    @State private var opened: UInt64?

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
                        message: "Each time a client uses a tool, it shows here with the client, the server, the time, and whether it worked.",
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
                                ActivityRow(
                                    call: CallFacts(event),
                                    isOpen: Binding(
                                        get: { opened == event.sequence },
                                        set: { opened = $0 ? event.sequence : nil }
                                    ),
                                    canShowServer: serverNames.contains(event.server ?? ""),
                                    run: run
                                )
                                    .listRowSeparator(.hidden)
                                    .listRowInsets(Metric.listRowInsets)
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
    @Binding var isOpen: Bool
    let canShowServer: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        Button { isOpen = true } label: {
            HStack(spacing: Metric.snug) {
                // The calling client's own icon, so a long list can be scanned
                // by picture rather than read line by line. Trouble replaces
                // the icon with a warning, so a failure is visible at a glance.
                ZStack {
                    if call.succeeded {
                        AppGlyph(target: call.callerTarget, name: call.caller, size: 22)
                    } else {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .font(.body)
                            .foregroundStyle(.orange)
                            .symbolRenderingMode(.hierarchical)
                    }
                }
                .frame(width: 22, height: 22)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: Metric.rowGap) {
                        if let server = call.server {
                            Text(server)
                                .font(.callout.weight(.medium))
                                .foregroundStyle(.secondary)
                            Text("·").foregroundStyle(.quaternary)
                        }
                        Text(call.tool)
                            .font(.callout.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Text(context).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: Metric.tight)
                VStack(alignment: .trailing, spacing: 1) {
                    Text(time).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                    Text(call.duration)
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(call.slow ? Color.orange : Color.secondary.opacity(0.7))
                }
            }
            .padding(.vertical, Metric.tight)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(call.result). \(call.tool)\(call.server.map { ", \($0)" } ?? ""), \(context), \(time), \(call.spokenDuration)"
        )
        .accessibilityHint("Shows what happened")
        .popover(isPresented: $isOpen, arrowEdge: .trailing) {
            CallDetail(call: call, canShowServer: canShowServer) { intent in
                isOpen = false
                run(intent)
            }
        }
    }

    /// Who called, then why it failed when it did.
    private var context: String {
        guard !call.succeeded else { return call.caller }
        return "\(call.caller) · \(call.reason ?? call.result)"
    }

    private var time: String {
        call.date.formatted(date: .omitted, time: .shortened)
    }
}

/// One call, opened: what ran, who asked, when, how long, and the result. A
/// failure says why and what to do next.
private struct CallDetail: View {
    let call: CallFacts
    let canShowServer: Bool
    let run: (PlugIntent) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(call.tool)
                    .font(.headline.monospaced())
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                Text(call.result)
                    .font(.callout)
                    .foregroundStyle(call.succeeded ? Color.secondary : .orange)
            }
            if !call.succeeded {
                ProblemNote(
                    title: call.reason ?? "This call \(call.result.lowercased()).",
                    advice: call.advice
                )
            }
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: Metric.regular, verticalSpacing: Metric.tight) {
                if let server = call.server { line("Server", server) }
                line("Client", call.caller)
                line("When", call.date.formatted(date: .abbreviated, time: .standard))
                line("Took", call.duration)
            }
            if canShowServer, let server = call.event.server {
                Button("Show Server") { run(.reveal(server: server)) }
            }
        }
        .padding(Metric.regular)
        .frame(width: 320, alignment: .leading)
    }

    private func line(_ name: String, _ value: String) -> some View {
        GridRow {
            Text(name).font(.callout).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }
}
