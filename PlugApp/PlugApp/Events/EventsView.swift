import PlugIPC
import SwiftUI

/// What Plug can tell a client without being asked. Each row is one event a
/// remote client can subscribe to, with how it is doing in a sentence.
struct EventsView: View {
    let model: AppModel
    @Bindable var router: Router
    @Binding var search: String
    let run: (PlugIntent) -> Void
    @State private var stopping: EventFacts?

    private var all: [EventFacts] { (model.snapshot.events ?? []).map(EventFacts.init(_:)) }
    private var events: [EventFacts] {
        let query = search.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return all }
        return all.filter {
            $0.name.localizedCaseInsensitiveContains(query)
                || ($0.tool?.localizedCaseInsensitiveContains(query) ?? false)
        }
    }

    private var summary: String? {
        guard !all.isEmpty else { return nil }
        let trouble = all.filter(\.health.needsAttention).count
        let count = all.count == 1 ? "1 event" : "\(all.count) events"
        return trouble == 0 ? count : "\(count), \(trouble) need attention"
    }

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "Events", detail: summary) {
                Button { run(.addWatch) } label: {
                    Label("Watch a Tool", systemImage: "plus")
                }
                .keyboardShortcut("n", modifiers: .command)
                .help("Watch a tool and send an event when its result changes")
                .disabled(!model.canMutate)
            }

            Group {
                if model.isLoadingInitialData {
                    LoadingPage(message: "Loading events…")
                } else if model.initialDataUnavailable {
                    UnavailablePage(item: "Events") { run(.reconnect) }
                } else if all.isEmpty {
                    EmptyPage(
                        title: "No events yet",
                        message: "Plug can watch a tool and tell a client when its result changes.",
                        symbol: "bell.badge",
                        actionTitle: model.canMutate ? "Watch a Tool" : nil,
                        actionIntent: model.canMutate ? .addWatch : nil,
                        run: run
                    )
                } else if events.isEmpty {
                    ContentUnavailableView.search(text: search)
                } else {
                    list
                }
            }
        }
        .sheet(isPresented: $router.isAddingWatch) {
            AddWatchView(model: model)
        }
        .confirmationDialog(
            "Stop watching \(stopping?.name ?? "")?",
            isPresented: Binding(get: { stopping != nil }, set: { if !$0 { stopping = nil } }),
            presenting: stopping
        ) { event in
            Button("Stop Watching", role: .destructive) { run(.removeWatch(event: event.name)) }
        } message: { event in
            Text(
                event.listeners == 0
                    ? "Plug stops calling the tool."
                    : "Plug stops calling the tool, and the clients listening stop hearing about it."
            )
        }
    }

    private var list: some View {
        // The sentences say "5 min ago", so a tick a minute keeps them true
        // between two changes of the snapshot.
        TimelineView(.periodic(from: .now, by: 60)) { context in
            let now = UInt64(max(0, context.date.timeIntervalSince1970))
            List {
                ForEach(events) { event in
                    EventRow(event: event, now: now, canMutate: model.canMutate) { stopping = event }
                        .listRowInsets(Metric.listRowInsets)
                        .listRowSeparator(.hidden)
                }
                Text("A remote client that signs in can subscribe to these. Plug sends the new result when it changes.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .listRowSeparator(.hidden)
            }
            .listStyle(.inset)
            .scrollContentBackground(.hidden)
            .frame(maxWidth: Metric.contentMaxWidth)
            .frame(maxWidth: .infinity)
        }
    }
}

private struct EventRow: View {
    let event: EventFacts
    let now: UInt64
    let canMutate: Bool
    let stop: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: Metric.snug) {
            Image(systemName: symbol)
                .foregroundStyle(color)
                .symbolRenderingMode(.hierarchical)
                .frame(width: 18, height: 18)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(event.name)
                    .font(.body.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Text(event.source)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if event.tool != nil {
                    Text(event.healthLine(now: now))
                        .font(.caption)
                        .foregroundStyle(event.health.needsAttention ? AnyShapeStyle(.orange) : AnyShapeStyle(.tertiary))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .layoutPriority(1)

            Spacer(minLength: Metric.snug)

            Text(event.listenerLine)
                .font(.caption.monospacedDigit())
                .foregroundStyle(event.listeners == 0 ? .tertiary : .secondary)
                .lineLimit(1)

            if event.canRemove {
                Button("Stop Watching", action: stop)
                    .controlSize(.small)
                    .disabled(!canMutate)
            }
        }
        .padding(.vertical, Metric.tight)
        .padding(.horizontal, Metric.tight)
        .hoverHighlight()
        .accessibilityElement(children: .contain)
        .contextMenu {
            Button("Copy Event Name") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(event.name, forType: .string)
            }
            if event.canRemove {
                Button("Stop Watching", role: .destructive, action: stop)
                    .disabled(!canMutate)
            }
        }
    }

    private var symbol: String {
        if event.health.needsAttention { return "exclamationmark.circle.fill" }
        return event.health == .waiting ? "circle.dotted" : "circle.fill"
    }

    private var color: Color {
        if event.health.needsAttention { return .orange }
        return event.health == .waiting ? .secondary : .green
    }
}
