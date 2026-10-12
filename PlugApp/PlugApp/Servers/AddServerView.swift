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

    /// What Plug understood about the server, as it was picked or pasted.
    @State private var facts: [DraftFact] = []
    /// False while the second step shows only what Plug understood. The
    /// whole form waits behind More Options until someone wants it.
    @State private var showsForm = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// The room kept under the paste box for the line about it, so the sheet
    /// does not jump when the line appears.
    private static let understoodHeight: CGFloat = 32

    var body: some View {
        SheetFrame(
            title: "Add Server",
            subtitle: subtitle,
            failure: failure,
            busy: saving,
            confirmTitle: form == nil ? "Continue" : "Add",
            confirmDisabled: form == nil ? draft == nil : !canAdd,
            confirm: { if form == nil { if let draft { start(draft) } } else { add() } },
            extra: {
                if form != nil {
                    Button("Back") { back() }
                        .disabled(saving)
                }
            }
        ) {
            // One sheet, two steps side by side: picking a server slides the
            // first away and the second in, and Back slides them home.
            ZStack(alignment: .top) {
                if let form {
                    details(form).transition(.step(.trailing))
                } else {
                    choose.transition(.step(.leading))
                }
            }
            .clipped()
        }
        .animation(reduceMotion ? nil : .snappy(duration: 0.32), value: form != nil)
        .animation(reduceMotion ? nil : .snappy(duration: 0.32), value: showsForm)
        .onChange(of: pasted) { _, _ in addressIsAPI = nil }
        .task(id: apiSpec) { await readOperations() }
        // The servers on offer get their makers' icons, the way added ones
        // do. A found icon is kept, so this asks each site about once a month.
        .task { await IconStore.shared.load(known.map { IconSource(name: $0.id, address: $0.address) }) }
    }

    private var subtitle: String {
        guard let form else {
            return known.isEmpty
                ? "Paste what the server's instructions give you."
                : "Pick one, or paste what the server's instructions give you."
        }
        return form.base.auth == "oauth"
            ? "Plug asks you to sign in after you add it."
            : "Check what Plug understood, then add it."
    }

    private var canAdd: Bool {
        guard let form else { return false }
        return form.isComplete && !trimmedName.isEmpty && choice?.problem == nil
    }

    // MARK: - Step 1: which server

    private var choose: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            if !known.isEmpty {
                VStack(alignment: .leading, spacing: Metric.tight) {
                    SectionLabel(text: "Popular Servers")
                    LazyVGrid(
                        columns: [GridItem(.flexible(), spacing: Metric.tight), GridItem(.flexible())],
                        spacing: Metric.tight
                    ) {
                        ForEach(known) { server in
                            Button { start(server.draft) } label: {
                                HStack(spacing: Metric.snug) {
                                    ServerGlyph(name: server.id, size: 26)
                                    VStack(alignment: .leading, spacing: Metric.hairline) {
                                        Text(server.title).font(.body)
                                        Text(server.summary)
                                            .font(PanelType.small)
                                            .foregroundStyle(.secondary)
                                            .multilineTextAlignment(.leading)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                            }
                            .buttonStyle(TileButtonStyle())
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
        facts = draft.facts
        let form = ServerForm(config: draft.config)
        // A server with nothing left to fill in shows only what Plug
        // understood. One that still needs a name or a field opens the form.
        showsForm = !form.isComplete || draft.name.isEmpty
        self.form = form
    }

    private func back() {
        form = nil
        failure = nil
    }

    // MARK: - Step 2: what Plug understood

    @ViewBuilder private func details(_ form: ServerForm) -> some View {
        if showsForm {
            ServerFormFields(
                form: Binding(
                    get: { self.form ?? form },
                    set: { self.form = $0 }
                ),
                name: $name
            ) {
                if apiSpec != nil {
                    Section {
                        operations
                    } header: {
                        SectionLabel(text: "Tools")
                    }
                }
            }
            .transition(.opacity)
        } else {
            VStack(alignment: .leading, spacing: Metric.regular) {
                HStack(alignment: .top, spacing: Metric.regular) {
                    ServerGlyph(name: trimmedName, size: 40)
                    VStack(alignment: .leading, spacing: Metric.tight) {
                        Text(trimmedName).font(.title3.weight(.semibold))
                        Grid(
                            alignment: .leadingFirstTextBaseline,
                            horizontalSpacing: Metric.snug,
                            verticalSpacing: Metric.rowGap
                        ) {
                            ForEach(facts) { fact in
                                GridRow {
                                    Text(fact.label).foregroundStyle(.secondary)
                                    Text(fact.value)
                                        .textSelection(.enabled)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                        }
                        .font(.callout)
                    }
                    Spacer(minLength: 0)
                }
                .padding(Metric.regular)
                .frame(maxWidth: .infinity, alignment: .leading)
                .nativeInsetSurface(.insetFill)

                if apiSpec != nil {
                    VStack(alignment: .leading, spacing: Metric.tight) {
                        SectionLabel(text: "Tools")
                        // A long API scrolls here; the sheet stays its size.
                        ViewThatFits(in: .vertical) {
                            operationsColumn
                            ScrollView { operationsColumn }
                        }
                        .frame(maxHeight: Self.operationsHeight)
                    }
                }

                // The same quiet disclosure Watch a Tool uses. Opening it
                // trades this card for the whole form.
                DisclosureGroup("More Options", isExpanded: $showsForm) {}
            }
            .padding(.horizontal, Metric.roomy)
            .transition(.opacity)
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

    private static let operationsHeight: CGFloat = 220

    private var operationsColumn: some View {
        VStack(alignment: .leading, spacing: Metric.tight) { operations }
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Which of an API's operations become tools. It sits under what Plug
    /// understood, and in the form's last section once that is open.
    @ViewBuilder private var operations: some View {
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
                                (Text(operation.name).font(.callout)
                                    + Text("  \(operation.method) \(operation.path)")
                                    .font(.callout)
                                    .foregroundStyle(.secondary))
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .toggleStyle(.checkbox)
                            .help(operation.summary)
                            .accessibilityLabel("\(operation.name), \(operation.method) \(operation.path)")
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } label: {
                Text("\(choice.chosen.count) of \(choice.api.operations.count) operations")
            }
            if let problem = choice.problem {
                InlineWarning(problem)
                    .font(PanelType.small)
            }
        } else if let choiceFailure {
            InlineWarning(choiceFailure)
                .font(.callout)
        } else {
            SheetLoading(label: "Reading the document", height: Self.understoodHeight)
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
