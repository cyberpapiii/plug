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
    let router: Router
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

    /// The room kept under the paste box for the line about it, so the sheet
    /// does not jump when the line appears.
    private static let understoodHeight: CGFloat = 32

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
            title: "Add Server",
            subtitle: known.isEmpty
                ? "Paste what the server's instructions give you."
                : "Pick one, or paste what the server's instructions give you.",
            confirmTitle: "Continue",
            confirmDisabled: draft == nil,
            confirm: { if let draft { start(draft) } }
        ) {
            Group {
                if !known.isEmpty {
                    VStack(alignment: .leading, spacing: Metric.tight) {
                        SectionLabel(text: "Popular Servers")
                        LazyVGrid(
                            columns: [GridItem(.flexible(), spacing: Metric.tight), GridItem(.flexible())],
                            spacing: Metric.tight
                        ) {
                            ForEach(known) { server in
                                Button { start(server.draft) } label: {
                                    VStack(alignment: .leading, spacing: Metric.hairline) {
                                        Text(server.title).font(.body)
                                        Text(server.summary)
                                            .font(PanelType.small)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.large)
                                .buttonBorderShape(.roundedRectangle(radius: Metric.corner))
                            }
                        }
                    }
                }

                VStack(alignment: .leading, spacing: Metric.tight) {
                    SectionLabel(text: known.isEmpty ? "Paste" : "Or Paste")
                    TextField(
                        "Paste a server",
                        text: $pasted,
                        prompt: Text("A setup block, a command, or a web address"),
                        axis: .vertical
                    )
                    .lineLimit(4...4)
                    .font(.body.monospaced())
                    understood
                        .frame(maxWidth: .infinity, minHeight: Self.understoodHeight, alignment: .topLeading)
                }
            }
            .padding(.horizontal, Metric.roomy)
        }
        .onChange(of: pasted) { _, _ in addressIsAPI = nil }
    }

    /// One line saying what the paste is, or why it could not be read.
    @ViewBuilder private var understood: some View {
        switch parse {
        case .empty:
            Color.clear
        case let .unreadable(reason):
            InlineWarning(reason)
                .font(.callout)
        case let .draft(draft):
            if draft.fromAddress {
                Picker("This address is", selection: Binding(
                    get: { draft.config.transport == "openapi" },
                    set: { addressIsAPI = $0 }
                )) {
                    Text("A Server").tag(false)
                    Text("A Web API").tag(true)
                }
                .pickerStyle(.segmented)
            } else {
                Label {
                    Text(
                        draft.name.isEmpty
                            ? "Plug read this. Continue to name it and check it."
                            : "Plug read this as \(draft.name). Continue to check it."
                    )
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                } icon: {
                    PlugIcon(.worked)
                        .foregroundStyle(StatusColor.working)
                }
                .font(.callout)
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
                title: "Add Server",
                subtitle: form.base.auth == "oauth"
                    ? "Plug asks you to sign in after you add it."
                    : "Check what Plug understood, then add it.",
                failure: failure,
                busy: saving,
                confirmTitle: "Add",
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
                ServerFormFields(
                    form: Binding(
                        get: { self.form ?? form },
                        set: { self.form = $0 }
                    ),
                    name: $name
                ) {
                    operations
                }
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

    /// The form's last section when the server is an API: which of its
    /// operations become tools.
    @ViewBuilder private var operations: some View {
        if apiSpec != nil {
            Section {
                if let choice {
                    DisclosureGroup(isExpanded: $showsOperations) {
                        VStack(alignment: .leading, spacing: Metric.rowGap) {
                            ForEach(choice.groups, id: \.tag) { group in
                                Text(group.tag)
                                    .font(PanelType.small.weight(.semibold))
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
                                        Text(operation.name).font(.callout)
                                            + Text("  \(operation.method) \(operation.path)")
                                            .font(.callout)
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
                    } label: {
                        Text("\(choice.chosen.count) of \(choice.api.operations.count) operations")
                    }
                } else if let choiceFailure {
                    InlineWarning(choiceFailure)
                        .font(.callout)
                } else {
                    SheetLoading(label: "Reading the document", height: Self.understoodHeight)
                }
            } header: {
                Text("Tools")
            } footer: {
                if let problem = choice?.problem {
                    InlineWarning(problem)
                }
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
        let store = form.keyStore.sent
        Task {
            do {
                try await model.performOperation {
                    .validateServer(authToken: $0, name: finalName, server: saved)
                }
                try await model.performOperation {
                    .addServer(authToken: $0, name: finalName, server: saved, secretStore: store)
                }
                saving = false
                router.reveal(server: finalName)
                dismiss()
            } catch {
                failure = error.localizedDescription
                saving = false
            }
        }
    }
}
