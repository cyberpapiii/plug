import PlugIPC
import SwiftUI

/// Watching a tool takes three choices: which server, which tool, how often.
/// Everything else has an answer already, so the sheet asks for those three
/// and keeps the rest out of the way.
struct AddWatchView: View {
    let model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var server = ""
    @State private var tool = ""
    @State private var everySecs: UInt64 = 300
    @State private var name = ""
    @State private var nameEdited = false
    @State private var arguments = ""
    @State private var showsAllTools = false
    @State private var showsOptions = false
    @State private var saving = false
    @State private var failure: String?
    @FocusState private var nameFocused: Bool
    @FocusState private var argumentsFocused: Bool

    /// Servers that have tools to watch right now.
    private var servers: [String] {
        model.situation.servers
            .filter { $0.enabled && !model.tools(for: $0.name).isEmpty }
            .map(\.name)
    }

    private var serverTools: [ToolFacts] {
        model.tools(for: server).sorted { own($0).localizedStandardCompare(own($1)) == .orderedAscending }
    }

    /// Watching calls the tool over and over, so only tools the server marks
    /// read-only are offered until the owner asks for the rest.
    private var offered: [ToolFacts] {
        showsAllTools ? serverTools : serverTools.filter(\.isReadOnly)
    }

    private var chosen: ToolFacts? { serverTools.first { own($0) == tool } }

    private func own(_ tool: ToolFacts) -> String { tool.ownName ?? tool.shortName }

    private var trimmedName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var parsedArguments: Result<[String: JSONValue], WatchDraft.ArgumentsError> {
        WatchDraft.arguments(from: arguments)
    }

    private var argumentsAreValid: Bool {
        if case .success = parsedArguments { return true }
        return false
    }

    private var canSave: Bool {
        chosen != nil && !trimmedName.isEmpty && argumentsAreValid && !saving
    }

    var body: some View {
        SheetFrame(
            title: "Watch a Tool",
            subtitle: "Plug calls the tool on a timer and sends an event when its result changes.",
            failure: failure,
            busy: saving,
            confirmTitle: servers.isEmpty ? nil : "Start Watching",
            confirmDisabled: !canSave,
            confirm: add
        ) {
            if servers.isEmpty {
                Text("No server has tools right now. Add a server, or wait for one to start.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, Metric.roomy)
            } else {
                form
            }
        }
        .onAppear {
            if server.isEmpty, let first = servers.first { server = first }
        }
        .onChange(of: server) { _, _ in
            failure = nil
            if !offered.contains(where: { own($0) == tool }) { tool = "" }
        }
        .onChange(of: showsAllTools) { _, _ in
            if !offered.contains(where: { own($0) == tool }) { tool = "" }
        }
        .onChange(of: tool) { _, _ in
            failure = nil
            if !nameEdited { name = WatchDraft.name(fromTool: tool) }
        }
    }

    private var form: some View {
        Form {
            Section {
                Picker("Server", selection: $server) {
                    ForEach(servers, id: \.self) { Text($0).tag($0) }
                }
                VStack(alignment: .leading, spacing: Metric.rowGap) {
                    Picker("Tool", selection: $tool) {
                        Text(offered.isEmpty ? "No tools to choose" : "Choose a tool").tag("")
                        ForEach(offered) { item in
                            Text(item.isReadOnly ? own(item) : "\(own(item)) (can change things)").tag(own(item))
                        }
                    }
                    .disabled(offered.isEmpty)
                    // Some servers write a page about a tool. Its first line
                    // is enough to recognise it by.
                    if let summary = chosen?.summary?.split(whereSeparator: \.isNewline).first {
                        caption(String(summary)).lineLimit(2)
                    } else if offered.isEmpty, !showsAllTools {
                        caption("Every \(server) tool can change things. Turn on the option below to pick one.")
                    }
                }
                // This decides what the Tool picker offers, so it sits right
                // under it and not inside More Options.
                VStack(alignment: .leading, spacing: Metric.rowGap) {
                    Toggle("Show tools that can change things", isOn: $showsAllTools)
                        .toggleStyle(.checkbox)
                    if showsAllTools {
                        caption("Plug calls a watched tool again and again. Only pick one that is safe to repeat.")
                    }
                }
                Picker("Check", selection: $everySecs) {
                    ForEach(WatchDraft.intervals, id: \.self) { secs in
                        Text(EventFacts.interval(secs).capitalizedFirst).tag(secs)
                    }
                }
                if chosen != nil, !trimmedName.isEmpty {
                    LabeledContent("Clients listen for") {
                        Text("\(server).\(trimmedName)")
                            .font(.body.monospaced())
                            .textSelection(.enabled)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
            }
            Section {
                DisclosureGroup("More Options", isExpanded: $showsOptions) {
                    TextField("Name", text: $name)
                        .focused($nameFocused)
                        .onChange(of: name) { _, _ in
                            if nameFocused { nameEdited = true }
                        }
                    VStack(alignment: .leading, spacing: Metric.rowGap) {
                        TextField(
                            "Arguments",
                            text: $arguments,
                            prompt: Text(#"{ "query": "is:unread" }"#),
                            axis: .vertical
                        )
                        .lineLimit(2...4)
                        .font(.body.monospaced())
                        .focused($argumentsFocused)
                        // The text is not valid JSON while it is being typed,
                        // so the complaint waits until the field is left.
                        if !argumentsAreValid, !argumentsFocused {
                            Label {
                                Text(#"Arguments have to be one JSON object, like { "query": "is:unread" }."#)
                                    .foregroundStyle(.secondary)
                            } icon: {
                                PlugIcon(.needsYou)
                                    .foregroundStyle(StatusColor.needsYou)
                            }
                            .font(PanelType.small)
                        } else {
                            caption("What the tool is called with, as JSON. Leave empty for none.")
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
    }

    private func caption(_ text: String) -> some View {
        Text(text)
            .font(PanelType.small)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func add() {
        guard let chosen, case let .success(values) = parsedArguments else { return }
        let watch = WatchConfig(
            name: trimmedName,
            server: server,
            tool: own(chosen),
            arguments: values,
            everySecs: everySecs,
            allowWrites: !chosen.isReadOnly
        )
        saving = true
        failure = nil
        Task {
            do {
                try await model.performOperation { .addWatch(authToken: $0, watch: watch) }
                saving = false
                dismiss()
            } catch {
                failure = error.localizedDescription
                saving = false
            }
        }
    }
}
