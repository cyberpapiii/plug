import PlugIPC
import SwiftUI

/// A second account for a server needs one thing from the owner: a short name
/// to tell the two apart. Plug copies the server and the copy signs in on its
/// own.
struct AddAccountView: View {
    let model: AppModel
    let router: Router
    let server: String
    @Environment(\.dismiss) private var dismiss
    @State private var typed = ""
    @State private var saving = false
    @State private var failure: String?

    private var label: String? { AccountDraft.label(from: typed) }

    private var usesOAuth: Bool {
        model.situation.servers.first { $0.name == server }?.usesOAuth ?? false
    }

    private var taken: Bool {
        guard let label else { return false }
        let name = AccountDraft.serverName(server: server, label: label)
        return model.situation.servers.contains { $0.name == name }
    }

    var body: some View {
        SheetFrame(
            title: "Add another account",
            subtitle: "Plug adds \(server) a second time under its own name. Both accounts stay available, each with its own tools.",
            failure: failure,
            busy: saving,
            confirmTitle: saving ? "Adding…" : "Add Account",
            confirmDisabled: !canSave,
            confirm: add
        ) {
            VStack(alignment: .leading, spacing: Metric.rowGap) {
                TextField("Account name, like personal or work", text: $typed)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { if canSave { add() } }
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(hintIsWarning ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                    .fixedSize(horizontal: false, vertical: true)
            }

            Label(
                usesOAuth
                    ? "The new server starts signed out. Sign in with the other account."
                    : "The new server starts with the same credentials. Edit it to use the other account.",
                systemImage: "person.2"
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var canSave: Bool { label != nil && !taken && !saving }

    private var hintIsWarning: Bool { taken || (label == nil && !typed.isEmpty) }

    private var hint: String {
        guard let label else {
            return typed.isEmpty
                ? "Lowercase letters and digits."
                : "Use lowercase letters and digits, starting with a letter, at most \(AccountDraft.longest)."
        }
        let name = AccountDraft.serverName(server: server, label: label)
        return taken ? "\(name) already exists." : "The new server is named \(name)."
    }

    private func add() {
        guard let label else { return }
        saving = true
        failure = nil
        Task {
            do {
                try await model.performOperation { .addAccount(authToken: $0, server: server, account: label) }
                saving = false
                router.reveal(server: AccountDraft.serverName(server: server, label: label))
                dismiss()
            } catch {
                failure = error.localizedDescription
                saving = false
            }
        }
    }
}
