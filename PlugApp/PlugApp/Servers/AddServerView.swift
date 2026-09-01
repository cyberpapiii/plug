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
    @FocusState private var focus: Field?

    private enum Field { case paste, name }

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.regular) {
            VStack(alignment: .leading, spacing: Metric.hairline) {
                Text("Add a server").font(.title2.weight(.semibold))
                Text("Paste the setup block from the server's instructions, a command, or a URL.")
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
                    .disabled(draft == nil || trimmedName.isEmpty || saving)
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .defaultFocus($focus, .paste)
        .interactiveDismissDisabled(saving)
        .onChange(of: pasted) { _, _ in
            failure = nil
            if !nameEdited, let draft { name = draft.name }
        }
    }

    private static let placeholder = """
    {
      "mcpServers": {
        "linear": { "command": "npx", "args": ["-y", "linear-mcp"] }
      }
    }
    """

    // MARK: - Understanding

    private var parse: ServerDraftParse { ServerDraftParser.parse(pasted) }

    private var draft: ServerDraft? {
        if case let .draft(value) = parse { return value }
        return nil
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
        guard let draft else { return }
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
