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
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading events…")
            } else if model.initialDataUnavailable {
                UnavailablePage(item: "Events") { run(.reconnect) }
            } else if all.isEmpty {
                EmptyPage(
                    title: "No events yet",
                    message: "Plug can watch a tool and tell a client when its result changes.",
                    symbol: "bell",
                    actionTitle: model.canMutate ? "Watch a Tool" : nil,
                    actionIntent: model.canMutate ? .addWatch : nil,
                    run: run
                )
            } else if events.isEmpty {
                ContentUnavailableView.search(text: search)
            } else {
                // The sentences say "5 min ago", so a tick a minute keeps
                // them true between two changes of the snapshot.
                TimelineView(.periodic(from: .now, by: 60)) { context in
                    let now = UInt64(max(0, context.date.timeIntervalSince1970))
                    ListDetail {
                        List(selection: $router.selectedEvent) {
                            ForEach(events) { event in
                                EventRow(event: event)
                                    .tag(event.name)
                                    .contextMenu {
                                        Button("Copy Event Name") { copy(event.name) }
                                        if event.canRemove {
                                            Button("Stop Watching…", role: .destructive) { stopping = event }
                                                .disabled(!model.canMutate)
                                        }
                                    }
                            }
                        }
                    } detail: {
                        if let selected {
                            EventDetail(event: selected, now: now, canMutate: model.canMutate) {
                                stopping = selected
                            }
                            .id(selected.name)
                        } else {
                            NoSelection(item: "Event")
                        }
                    }
                }
            }
        }
        .navigationSubtitle(summary ?? "")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { run(.addWatch) } label: {
                    Label("Watch a Tool", systemImage: "plus")
                }
                .help("Watch a tool and send an event when its result changes")
                .disabled(!model.canMutate)
            }
        }
        .onChange(of: events.map(\.name), initial: true) { keepSelectionVisible() }
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

    private var selected: EventFacts? { events.first { $0.name == router.selectedEvent } }

    private func keepSelectionVisible() {
        let names = events.map(\.name)
        if let current = router.selectedEvent, names.contains(current) { return }
        Task { @MainActor in
            await Task.yield()
            router.selectedEvent = names.first
        }
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

private extension EventFacts {
    var symbol: String {
        if health.needsAttention { return "exclamationmark.circle.fill" }
        return health == .waiting ? "circle.dotted" : "circle.fill"
    }

    var color: Color {
        if health.needsAttention { return .orange }
        return health == .waiting ? .secondary : .green
    }
}

private struct EventRow: View {
    let event: EventFacts

    var body: some View {
        HStack(spacing: Metric.tight) {
            Image(systemName: event.symbol)
                .font(event.health.needsAttention ? .body : .caption2)
                .foregroundStyle(event.color)
                .symbolRenderingMode(.hierarchical)
                .frame(width: 18, height: 18)
                .accessibilityHidden(true)
            Text(event.name)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }
}

/// One event in full: where it comes from, how it is doing, and who hears it.
private struct EventDetail: View {
    let event: EventFacts
    let now: UInt64
    let canMutate: Bool
    let stop: () -> Void

    var body: some View {
        DetailForm {
            Section {
                DetailHeader(title: event.name, subtitle: event.source, monospaced: true) {
                    Image(systemName: event.health.needsAttention ? "exclamationmark.circle.fill" : "bell.fill")
                        .font(.title2)
                        .symbolRenderingMode(.hierarchical)
                        .foregroundStyle(event.health.needsAttention ? Color.orange : .secondary)
                        .accessibilityHidden(true)
                } controls: {
                    if event.canRemove {
                        Button("Stop Watching…", action: stop)
                            .disabled(!canMutate)
                    }
                }
            } footer: {
                Text("A remote client that signs in can subscribe to this. Plug sends the new result when it changes.")
            }
            Section {
                if event.tool != nil {
                    LabeledContent("Status") {
                        Text(event.healthLine(now: now))
                            .foregroundStyle(event.health.needsAttention ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                            .multilineTextAlignment(.trailing)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                LabeledContent("Server", value: event.server)
                LabeledContent("Listening", value: event.listenerLine)
            }
        }
    }
}
