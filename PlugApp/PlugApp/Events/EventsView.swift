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
    /// Kept apart from `stopping` so the title still names the event while
    /// the dialog closes.
    @State private var stoppingName = ""

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
        return trouble == 0 ? count : "\(count) · \(trouble) \(trouble == 1 ? "needs" : "need") attention"
    }

    private var serverNames: Set<String> { Set(model.situation.servers.map(\.name)) }

    var body: some View {
        Group {
            if model.isLoadingInitialData {
                LoadingPage(message: "Loading events")
            } else if model.initialDataUnavailable {
                UnavailablePage(verdict: model.verdict, run: run)
            } else if all.isEmpty {
                EmptyPage(
                    title: "No Events",
                    message: "Plug can watch a tool and tell a client when its result changes.",
                    symbol: AppSection.events.symbol,
                    actionTitle: model.canMutate ? "Watch a Tool…" : nil,
                    actionIntent: model.canMutate ? .addWatch : nil,
                    run: run
                )
            } else if events.isEmpty {
                ContentUnavailableView.search(text: search)
            } else {
                // The detail says "5 min ago", so a tick a minute keeps it
                // true between two changes of the snapshot.
                TimelineView(.periodic(from: .now, by: 60)) { context in
                    let now = UInt64(max(0, context.date.timeIntervalSince1970))
                    ListDetail {
                        List(selection: $router.selectedEvent) {
                            ForEach(events) { event in
                                EventRow(event: event)
                                    .tag(event.name)
                                    .contextMenu {
                                        Button("Copy Event Name") { copy(event.name) }
                                        if event.canRemove, model.canMutate {
                                            Button("Stop Watching…", role: .destructive) { askToStop(event) }
                                        }
                                    }
                            }
                        }
                        .onDeleteCommand {
                            if model.canMutate, let selected, selected.canRemove { askToStop(selected) }
                        }
                    } detail: {
                        if let selected {
                            EventDetail(
                                event: selected,
                                now: now,
                                canMutate: model.canMutate,
                                showServer: serverNames.contains(selected.server)
                                    ? { run(.reveal(server: selected.server)) }
                                    : nil
                            ) {
                                askToStop(selected)
                            }
                            .id(selected.name)
                        } else {
                            NoSelection(item: "Event", symbol: AppSection.events.symbol)
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
            "Stop watching \(stoppingName)?",
            isPresented: Binding(get: { stopping != nil }, set: { if !$0 { stopping = nil } }),
            titleVisibility: .visible,
            presenting: stopping
        ) { event in
            Button("Stop Watching", role: .destructive) { run(.removeWatch(event: event.name)) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Plug stops calling the tool, and listening clients stop getting this event.")
        }
    }

    private var selected: EventFacts? { events.first { $0.name == router.selectedEvent } }

    private func askToStop(_ event: EventFacts) {
        stoppingName = event.name
        stopping = event
    }

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

/// An event's state as one glyph, in the same symbols a server's state uses.
/// The words beside it say the same thing, so it is hidden from VoiceOver.
private struct EventGlyph: View {
    let health: EventFacts.Health
    var large = false

    var body: some View {
        Group {
            if health.needsAttention {
                symbol("exclamationmark.triangle.fill").foregroundStyle(.orange)
            } else if health == .waiting {
                symbol("circle.dotted").foregroundStyle(.secondary)
            } else {
                ZStack {
                    Circle()
                        .fill(Color.green.opacity(0.12))
                        .frame(width: large ? 28 : 14, height: large ? 28 : 14)
                    Circle()
                        .fill(Color.green)
                        .frame(width: large ? 12 : 7, height: large ? 12 : 7)
                }
            }
        }
        .frame(width: large ? Metric.glyphSlot : 18, height: large ? Metric.glyphSlot : 18)
        .accessibilityHidden(true)
    }

    private func symbol(_ name: String) -> some View {
        Image(systemName: name)
            .font(large ? .title2 : .body)
            .symbolRenderingMode(.hierarchical)
    }
}

private struct EventRow: View {
    let event: EventFacts

    var body: some View {
        HStack(spacing: Metric.tight) {
            EventGlyph(health: event.health)
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text(event.name)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(event.stateWord)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(event.name), \(event.stateWord)")
    }
}

/// One event in full: where it comes from, how it is doing, and who hears it.
private struct EventDetail: View {
    let event: EventFacts
    let now: UInt64
    let canMutate: Bool
    /// Nil when the server is not one Plug can show.
    let showServer: (() -> Void)?
    let stop: () -> Void

    var body: some View {
        DetailForm {
            Section {
                DetailHeader(title: event.name, subtitle: event.stateWord) {
                    EventGlyph(health: event.health, large: true)
                } controls: {
                    if event.canRemove {
                        Button("Stop Watching…", action: stop)
                            .disabled(!canMutate)
                    }
                }
                if event.health.needsAttention {
                    ProblemNote(title: event.healthLine(now: now))
                }
            }
            if event.tool != nil, !event.health.needsAttention {
                Section {
                    LabeledContent("Last Checked", value: EventFacts.ago(event.lastChecked, now: now).capitalizedFirst)
                    LabeledContent(
                        "Last Changed",
                        value: event.lastChanged == nil
                            ? "Not yet"
                            : EventFacts.ago(event.lastChanged, now: now).capitalizedFirst
                    )
                }
            }
            Section {
                if let tool = event.tool {
                    LabeledContent("Tool", value: tool)
                }
                LabeledContent("Server") {
                    HStack(spacing: Metric.tight) {
                        Text(event.server)
                        if let showServer {
                            Button(action: showServer) {
                                Image(systemName: "arrow.right.circle")
                            }
                            .buttonStyle(.borderless)
                            .foregroundStyle(.secondary)
                            .help("Show Server")
                            .accessibilityLabel("Show Server")
                        }
                    }
                }
                if let everySecs = event.everySecs {
                    LabeledContent("Checks", value: EventFacts.interval(everySecs).capitalizedFirst)
                }
                LabeledContent("Clients", value: event.listenerLine)
                LabeledContent("Full Name") {
                    Text(event.name)
                        .font(.body.monospaced())
                        .textSelection(.enabled)
                }
            } header: {
                Text("Details")
            } footer: {
                Text("Clients connected over the network can listen for this event. Plug sends the new result each time it changes.")
            }
        }
    }
}
