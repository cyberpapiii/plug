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
    @FocusState private var pasteFocused: Bool

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
                .frame(height: 132)
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
                .focused($pasteFocused)
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
                    .disabled(draft == nil || name.isEmpty || saving)
            }
        }
        .padding(Metric.roomy)
        .frame(width: 520)
        .onAppear { pasteFocused = true }
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
            Text("Nothing pasted yet.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity, alignment: .leading)
        case let .unreadable(reason):
            Label(reason, systemImage: "questionmark.circle")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        case let .draft(draft):
            VStack(alignment: .leading, spacing: Metric.snug) {
                HStack(spacing: Metric.snug) {
                    Text("Name").font(.callout).foregroundStyle(.secondary)
                    TextField("Name", text: $name)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: name) { _, _ in nameEdited = true }
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

    private func add() {
        guard let draft else { return }
        saving = true
        failure = nil
        let finalName = name.trimmingCharacters(in: .whitespaces)
        Task {
            await model.perform { .validateServer(authToken: $0, name: finalName, server: draft.config) }
            if let error = model.lastError {
                failure = error
                saving = false
                return
            }
            await model.perform { .addServer(authToken: $0, name: finalName, server: draft.config) }
            saving = false
            if let error = model.lastError {
                failure = error
            } else {
                dismiss()
            }
        }
    }
}
