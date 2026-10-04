import PlugIPC
import SwiftUI

/// Adding a server, in two steps.
///
/// First, which server: pick one Plug already knows, or paste what the
/// server's instructions give you. A person with nothing to paste still has a
/// next step. Then the same form Edit shows, filled in, so what Plug understood
/// can be read and corrected before it is saved.
struct AddServerView: View {
    let model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var pasted = ""
    @State private var name = ""
    /// The filled-in form, once a server was picked or a paste was understood.
    @State private var form: ServerForm?
    @State private var saving = false
    @State private var failure: String?
    /// The person's answer to "is this address an API?", when they gave one.
    @State private var addressIsAPI: Bool?
    /// The operations of the API being added, once the daemon has read its
    /// document, and which of them the person kept.
    @State private var choice: APIOperationChoice?
    @State private var choiceFailure: String?
    @State private var showsOperations = false
    @FocusState private var pasteFocused: Bool

    var body: some View {
        if form != nil {
            details
        } else {
            choose
        }
    }

    // MARK: - Step 1: which server

    private var choose: some View {
        SheetFrame(
            title: "Add a server",
            subtitle: "Pick one, or paste what the server's instructions give you.",
            confirmTitle: "Continue",
            confirmDisabled: draft == nil,
            confirm: { if let draft { start(draft) } }
        ) {
            if !known.isEmpty {
                VStack(alignment: .leading, spacing: Metric.tight) {
                    SectionLabel(text: "Popular servers")
                    LazyVGrid(
                        columns: [GridItem(.flexible(), spacing: Metric.tight), GridItem(.flexible())],
                        spacing: Metric.tight
                    ) {
                        ForEach(known) { server in
                            Button { start(server.draft) } label: {
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(server.title).font(.callout.weight(.medium))
                                    Text(server.summary)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, Metric.snug)
                                .padding(.vertical, Metric.tight)
                                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 7))
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .hoverHighlight()
                            .accessibilityLabel("\(server.title), \(server.summary)")
                        }
                    }
                }
            }

            VStack(alignment: .leading, spacing: Metric.tight) {
                SectionLabel(text: known.isEmpty ? "Paste" : "Or paste")
                TextEditor(text: $pasted)
                    .font(.system(.callout, design: .monospaced))
                    .scrollContentBackground(.hidden)
                    .padding(Metric.snug)
                    .frame(height: 96)
                    .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: Metric.corner))
                    .overlay(alignment: .topLeading) {
                        if pasted.isEmpty {
                            Text("A setup block, a command, a web address, or the address of an API's OpenAPI document")
                                .font(.callout)
                                .foregroundStyle(.tertiary)
                                .padding(Metric.snug + 4)
                                .allowsHitTesting(false)
                        }
                    }
                    .focused($pasteFocused)
                    .accessibilityLabel("Paste a server")
                understood
            }
        }
        .onChange(of: pasted) { _, _ in addressIsAPI = nil }
    }

    /// One line saying what the paste is, or why it could not be read.
    @ViewBuilder private var understood: some View {
        switch parse {
        case .empty:
            EmptyView()
        case let .unreadable(reason):
            Label(reason, systemImage: "questionmark.circle")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        case let .draft(draft):
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
            } else {
                Label("Plug can read this. Continue to check it.", systemImage: "checkmark.circle")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var known: [KnownServer] {
        KnownServer.notYetAdded(names: model.situation.servers.map(\.name))
    }

    private var parse: ServerDraftParse {
        ServerDraftParser.parse(pasted, addressIsAPI: addressIsAPI)
    }

    private var draft: ServerDraft? {
        if case let .draft(value) = parse { return value }
        return nil
    }

    private func start(_ draft: ServerDraft) {
        failure = nil
        name = draft.name
        form = ServerForm(config: draft.config)
    }

    // MARK: - Step 2: the form

    @ViewBuilder private var details: some View {
        if let form {
            SheetFrame(
                title: "Add a server",
                subtitle: form.base.auth == "oauth"
                    ? "Plug asks you to sign in after you add it."
                    : "Check what Plug understood, then add it.",
                failure: failure,
                busy: saving,
                confirmTitle: saving ? "Adding…" : "Add Server",
                confirmDisabled: !form.isComplete || trimmedName.isEmpty || choice?.problem != nil,
                confirm: add,
                extra: {
                    Button("Back") {
                        self.form = nil
                        failure = nil
                    }
                    .disabled(saving)
                }
            ) {
                HStack(spacing: Metric.snug) {
                    Text("Name").font(.callout).foregroundStyle(.secondary)
                    TextField("Name", text: $name)
                        .textFieldStyle(.roundedBorder)
                }
                ServerFormFields(form: Binding(
                    get: { self.form ?? form },
                    set: { self.form = $0 }
                ))
                operations
            }
            .task(id: apiSpec) { await readOperations() }
        }
    }

    /// The document to read operations from, when the server is an API.
    private var apiSpec: String? {
        guard let form, form.isAPI else { return nil }
        return form.base.spec
    }

    /// Asks the daemon what the API offers. A failure here does not block
    /// adding: the server reports it later.
    private func readOperations() async {
        choice = nil
        choiceFailure = nil
        guard let spec = apiSpec else { return }
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

    // MARK: - Saving

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func add() {
        guard let form else { return }
        var config = form.config
        if let choice { config.operations = choice.setting }
        let finalName = trimmedName
        guard !finalName.isEmpty else { return }
        saving = true
        failure = nil
        let saved = config
        Task {
            do {
                try await model.performOperation {
                    .validateServer(authToken: $0, name: finalName, server: saved)
                }
                try await model.performOperation {
                    .addServer(authToken: $0, name: finalName, server: saved)
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
