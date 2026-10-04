import PlugIPC
import SwiftUI

/// Adding a server is the one moment where Plug can be delightful, and the way
/// people actually acquire servers is copying a block out of a README. So this
/// asks for exactly that — paste anything — and shows what it understood before
/// committing. No field-by-field transcription of something already on the
/// clipboard.
struct AddServerView: View {
    let model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var pasted = ""
    @State private var name = ""
    @State private var nameEdited = false
    @State private var saving = false
    @State private var failure: String?
    /// The person's answer to "is this address an API?", when they gave one.
    @State private var addressIsAPI: Bool?
    /// The operations of the API being added, once the daemon has read its
    /// document, and which of them the person kept.
    @State private var choice: APIOperationChoice?
    @State private var choiceFailure: String?
    @State private var showsOperations = false
    @FocusState private var focus: Field?

    private enum Field { case paste, name }

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Add a server").font(.title2.weight(.semibold))
                Text("Paste the setup block from the server's instructions, a command, a URL, or the address of an API's OpenAPI document.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            TextEditor(text: $pasted)
                .font(.system(.callout, design: .monospaced))
                .scrollContentBackground(.hidden)
                .padding(Metric.snug)
                .frame(height: 144)
                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: Metric.corner))
                .overlay(alignment: .topLeading) {
                    if pasted.isEmpty {
                        Text(Self.placeholder)
                            .font(.system(.callout, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .padding(Metric.snug + 4)
                            .allowsHitTesting(false)
                    }
                }
                .focused($focus, equals: .paste)
                .accessibilityLabel("Server definition")

            preview

            HStack(spacing: Metric.snug) {
                if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(saving ? "Adding…" : "Add Server") { add() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(draft == nil || trimmedName.isEmpty || saving || choice?.problem != nil)
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .defaultFocus($focus, .paste)
        .interactiveDismissDisabled(saving)
        .onChange(of: pasted) { _, _ in
            failure = nil
            addressIsAPI = nil
            if !nameEdited, let draft { name = draft.name }
        }
        .task(id: apiSpec) { await readOperations() }
    }

    private static let placeholder = """
    {
      "mcpServers": {
        "linear": { "command": "npx", "args": ["-y", "linear-mcp"] }
      }
    }
    """

    // MARK: - Understanding

    private var parse: ServerDraftParse {
        ServerDraftParser.parse(pasted, addressIsAPI: addressIsAPI)
    }

    private var draft: ServerDraft? {
        if case let .draft(value) = parse { return value }
        return nil
    }

    /// The document to read operations from, when the draft is an API.
    private var apiSpec: String? {
        guard let draft, draft.config.transport == "openapi" else { return nil }
        return draft.config.spec
    }

    /// Waits for typing to settle, then asks the daemon what the API offers.
    /// A failure here does not block adding: the server reports it later.
    private func readOperations() async {
        choice = nil
        choiceFailure = nil
        guard let spec = apiSpec else { return }
        try? await Task.sleep(for: .milliseconds(500))
        guard !Task.isCancelled else { return }
        do {
            let api = try await model.describeAPI(spec)
            guard !Task.isCancelled else { return }
            let read = APIOperationChoice(api: api)
            choice = read
            showsOperations = read.problem != nil
        } catch {
            guard !Task.isCancelled else { return }
            choiceFailure = error.localizedDescription
        }
    }

    @ViewBuilder private var operations: some View {
        if let choice {
            DisclosureGroup(isExpanded: $showsOperations) {
                ScrollView {
                    VStack(alignment: .leading, spacing: Metric.hairline) {
                        ForEach(choice.groups, id: \.tag) { group in
                            Text(group.tag)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .padding(.top, Metric.hairline)
                            ForEach(group.operations) { operation in
                                Toggle(isOn: Binding(
                                    get: { choice.chosen.contains(operation.name) },
                                    set: { on in
                                        if on {
                                            self.choice?.chosen.insert(operation.name)
                                        } else {
                                            self.choice?.chosen.remove(operation.name)
                                        }
                                    }
                                )) {
                                    Text(operation.name).font(.caption.monospaced())
                                        + Text("  \(operation.method) \(operation.path)")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                .toggleStyle(.checkbox)
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .help(operation.summary)
                                .accessibilityLabel("\(operation.name), \(operation.method) \(operation.path)")
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 160)
            } label: {
                HStack(spacing: Metric.snug) {
                    Text("\(choice.chosen.count) of \(choice.api.operations.count) operations")
                        .font(.caption)
                    if let problem = choice.problem {
                        Text(problem).font(.caption).foregroundStyle(.orange)
                    }
                }
            }
        } else if let choiceFailure {
            Label(choiceFailure, systemImage: "exclamationmark.triangle")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
        } else if apiSpec != nil {
            HStack(spacing: Metric.snug) {
                ProgressView().controlSize(.small)
                Text("Reading the document…").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var preview: some View {
        switch parse {
        case .empty:
            EmptyView()
        case let .unreadable(reason):
            Label(reason, systemImage: "questionmark.circle")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        case let .draft(draft):
            VStack(alignment: .leading, spacing: Metric.snug) {
                SectionLabel(text: "Detected server")
                HStack(spacing: Metric.snug) {
                    Text("Name").font(.callout).foregroundStyle(.secondary)
                    TextField("Name", text: $name)
                        .textFieldStyle(.roundedBorder)
                        .focused($focus, equals: .name)
                        .onChange(of: name) { _, _ in
                            if focus == .name { nameEdited = true }
                        }
                }
                if draft.fromAddress {
                    Picker("This address is", selection: Binding(
                        get: { draft.config.transport == "openapi" },
                        set: { addressIsAPI = $0 }
                    )) {
                        Text("A server").tag(false)
                        Text("An API's OpenAPI document").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .font(.callout)
                }
                ForEach(draft.facts) { fact in
                    HStack(alignment: .firstTextBaseline, spacing: Metric.snug) {
                        Text(fact.label)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .frame(width: 84, alignment: .leading)
                        Text(fact.value)
                            .font(.caption.monospaced())
                            .lineLimit(2)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                    }
                }
                operations
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Metric.regular)
            .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: Metric.corner))
            .transition(.opacity)
        }
    }

    // MARK: - Saving

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func add() {
        guard var draft else { return }
        if let choice { draft.config.operations = choice.setting }
        saving = true
        failure = nil
        let finalName = trimmedName
        guard !finalName.isEmpty else {
            saving = false
            return
        }
        Task {
            do {
                try await model.performOperation {
                    .validateServer(authToken: $0, name: finalName, server: draft.config)
                }
                try await model.performOperation {
                    .addServer(authToken: $0, name: finalName, server: draft.config)
                }
                saving = false
                dismiss()
            } catch {
                failure = error.localizedDescription
                saving = false
            }
        }
    }
}
