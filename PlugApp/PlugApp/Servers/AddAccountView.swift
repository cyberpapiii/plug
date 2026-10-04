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
            title: "Add Another Account",
            subtitle: "Plug adds \(server) again under its own name.",
            failure: failure,
            busy: saving,
            confirmTitle: "Add",
            confirmDisabled: !canSave,
            confirm: add
        ) {
            Form {
                Section {
                    TextField("Account", text: $typed, prompt: Text("work"))
                } footer: {
                    hint
                }
            }
            .formStyle(.grouped)
        }
    }

    private var canSave: Bool { label != nil && !taken && !saving }

    /// What the new server will be called, or why the name will not do.
    @ViewBuilder private var hint: some View {
        if let label {
            let name = AccountDraft.serverName(server: server, label: label)
            if taken {
                InlineWarning("\(name) already exists.")
            } else {
                Text(
                    usesOAuth
                        ? "The new server is named \(name). It starts signed out."
                        : "The new server is named \(name). It starts with the same key; edit it to use the other account."
                )
            }
        } else if typed.isEmpty {
            Text("Lowercase letters and digits.")
        } else {
            InlineWarning("Use lowercase letters and digits, starting with a letter, at most \(AccountDraft.longest).")
        }
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
