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
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Watch a tool").font(.title2.weight(.semibold))
                Text("Plug calls the tool on a timer and sends an event when its result changes.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if servers.isEmpty {
                Label("No server has tools right now. Add a server, or wait for one to start.", systemImage: "questionmark.circle")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                form
            }

            HStack(spacing: Metric.snug) {
                if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(saving ? "Starting…" : "Start Watching") { add() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canSave)
            }
        }
        .padding(Metric.roomy)
        .frame(width: 480)
        .interactiveDismissDisabled(saving)
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

    @ViewBuilder private var form: some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: Metric.snug, verticalSpacing: Metric.snug) {
            GridRow {
                label("Server")
                Picker("Server", selection: $server) {
                    ForEach(servers, id: \.self) { Text($0).tag($0) }
                }
                .labelsHidden()
            }
            GridRow {
                label("Tool")
                VStack(alignment: .leading, spacing: Metric.rowGap) {
                    Picker("Tool", selection: $tool) {
                        Text(offered.isEmpty ? "No tools to choose" : "Choose a tool").tag("")
                        ForEach(offered) { item in
                            Text(item.isReadOnly ? own(item) : "\(own(item)) (can change things)").tag(own(item))
                        }
                    }
                    .labelsHidden()
                    .disabled(offered.isEmpty)
                    // Some servers write a page about a tool. Its first line
                    // is enough to recognise it by.
                    if let summary = chosen?.summary?.split(whereSeparator: \.isNewline).first {
                        Text(String(summary))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    } else if offered.isEmpty, !showsAllTools {
                        Text("\(server) marks none of its tools read-only.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            GridRow {
                label("Check")
                Picker("Check", selection: $everySecs) {
                    ForEach(WatchDraft.intervals, id: \.self) { secs in
                        Text(EventFacts.interval(secs).capitalizedFirst).tag(secs)
                    }
                }
                .labelsHidden()
                .fixedSize()
            }
        }

        if chosen != nil, !trimmedName.isEmpty {
            HStack(spacing: Metric.tight) {
                Text("Clients subscribe to").font(.caption).foregroundStyle(.secondary)
                Text("\(server).\(trimmedName)")
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }

        DisclosureGroup("More options", isExpanded: $showsOptions) {
            VStack(alignment: .leading, spacing: Metric.snug) {
                HStack(spacing: Metric.snug) {
                    label("Name")
                    TextField("Name", text: $name)
                        .textFieldStyle(.roundedBorder)
                        .focused($nameFocused)
                        .onChange(of: name) { _, _ in
                            if nameFocused { nameEdited = true }
                        }
                }
                VStack(alignment: .leading, spacing: Metric.rowGap) {
                    Text("Arguments").font(.callout).foregroundStyle(.secondary)
                    TextEditor(text: $arguments)
                        .font(.system(.callout, design: .monospaced))
                        .scrollContentBackground(.hidden)
                        .padding(Metric.tight)
                        .frame(height: 64)
                        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: Metric.corner))
                        .overlay(alignment: .topLeading) {
                            if arguments.isEmpty {
                                Text(#"{ "query": "is:unread" }"#)
                                    .font(.system(.callout, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                                    .padding(Metric.tight + 4)
                                    .allowsHitTesting(false)
                            }
                        }
                        .accessibilityLabel("Arguments")
                    Text(
                        argumentsAreValid
                            ? "What the tool is called with, as JSON. Leave empty for none."
                            : "Arguments have to be one JSON object, like { \"query\": \"is:unread\" }."
                    )
                    .font(.caption)
                    .foregroundStyle(argumentsAreValid ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.orange))
                }
                Toggle("Show tools that can change things", isOn: $showsAllTools)
                if showsAllTools {
                    Text("Plug calls a watched tool again and again. Only pick one that is safe to repeat.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.top, Metric.snug)
        }
        .font(.callout)
    }

    private func label(_ text: String) -> some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .gridColumnAlignment(.trailing)
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

private extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}
