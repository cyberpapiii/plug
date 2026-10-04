import PlugIPC
import SwiftUI

/// The fields of one server, as a person fills them in.
///
/// Adding and editing are the same form. It starts from a complete server
/// definition and gives one back, so settings the form has no field for stay
/// as they were.
struct ServerForm: Equatable, Sendable {
    var isRemote: Bool
    var command: String
    var arguments: String
    var address: String
    /// A key typed now. Empty keeps whatever the server already has.
    var key = ""
    var removeKey = false
    var settings: String
    private(set) var base: ServerConfig

    init(config: ServerConfig) {
        base = config
        isRemote = config.transport.lowercased() != "stdio"
        command = config.command ?? ""
        arguments = Self.renderArguments(config.args)
        address = config.url ?? ""
        settings = config.env
            .sorted { $0.key.localizedStandardCompare($1.key) == .orderedAscending }
            .map { "\($0.key)=\($0.value)" }
            .joined(separator: "\n")
    }

    var hasKey: Bool { base.authToken != nil }
    var isAPI: Bool { base.transport == "openapi" }

    /// Why the variables cannot be saved as typed, if they cannot.
    var settingsProblem: String? {
        let lines = settings.split(whereSeparator: { $0 == "\n" })
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let malformed = lines.contains { line in
            guard let split = line.firstIndex(of: "=") else { return true }
            return line[line.startIndex..<split].trimmingCharacters(in: .whitespaces).isEmpty
        }
        return malformed ? "Each line needs NAME=value." : nil
    }

    var isComplete: Bool {
        guard settingsProblem == nil else { return false }
        // An API server may leave the address to its document.
        if isAPI, isRemote { return true }
        return isRemote
            ? !address.trimmingCharacters(in: .whitespaces).isEmpty
            : !command.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The server these fields describe.
    var config: ServerConfig {
        var config = base
        if isRemote {
            // A server that was already remote keeps its kind: saving must not
            // turn an SSE server or an API server into a plain HTTP one.
            if config.transport.lowercased() == "stdio" { config.transport = "http" }
            config.command = nil
            config.args = []
            let trimmed = address.trimmingCharacters(in: .whitespaces)
            config.url = trimmed.isEmpty && isAPI ? nil : trimmed
            let typed = key.trimmingCharacters(in: .whitespaces)
            if !typed.isEmpty {
                config.authToken = typed
            } else if removeKey {
                config.authToken = nil
            }
        } else {
            config.transport = "stdio"
            config.command = command.trimmingCharacters(in: .whitespaces)
            config.args = ServerDraftParser.tokenize(arguments)
            config.url = nil
            config.authToken = nil
            config.auth = nil
            config.oauthClientID = nil
            config.oauthScopes = nil
        }
        config.env = Self.parseSettings(settings)
        return config
    }

    static func parseSettings(_ text: String) -> [String: String] {
        var result: [String: String] = [:]
        for line in text.split(whereSeparator: { $0 == "\n" }) {
            let entry = line.trimmingCharacters(in: .whitespaces)
            guard let split = entry.firstIndex(of: "=") else { continue }
            let key = String(entry[entry.startIndex..<split]).trimmingCharacters(in: .whitespaces)
            let value = String(entry[entry.index(after: split)...])
                .trimmingCharacters(in: .whitespaces)
            guard !key.isEmpty else { continue }
            result[key] = value
        }
        return result
    }

    static func renderArguments(_ arguments: [String]) -> String {
        arguments.map { argument in
            guard argument.isEmpty || argument.contains(where: { $0.isWhitespace || "'\"\\".contains($0) }) else {
                return argument
            }
            return "'\(argument.replacingOccurrences(of: "'", with: "'\\''"))'"
        }.joined(separator: " ")
    }
}

/// The form itself, shared by Add Server and Edit: one grouped form. Add
/// Server passes the name, and its own sections go last.
struct ServerFormFields<Extra: View>: View {
    @Binding var form: ServerForm
    /// The server's name, when the sheet lets it be chosen.
    var name: Binding<String>?
    @ViewBuilder var extra: Extra

    /// A server you sign in to has no key to type.
    private var signsIn: Bool { form.base.auth == "oauth" }

    var body: some View {
        Form {
            // An API is described by a document; it has no "where".
            if name != nil || !form.isAPI {
                Section {
                    if let name {
                        TextField("Name", text: name)
                    }
                    if !form.isAPI {
                        Picker("Where It Runs", selection: $form.isRemote) {
                            Text("On This Mac").tag(false)
                            Text("Over the Network").tag(true)
                        }
                        .pickerStyle(.segmented)
                    }
                }
            }

            Section {
                if form.isRemote {
                    if form.isAPI, let document = form.base.spec {
                        LabeledContent("Document") {
                            Text(document)
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .textSelection(.enabled)
                        }
                    }
                    TextField(
                        "Address",
                        text: $form.address,
                        prompt: Text(form.isAPI ? "The one the document names" : "https://example.com/mcp")
                    )
                    if !signsIn {
                        SecureField(
                            "Key",
                            text: $form.key,
                            prompt: Text(form.hasKey && !form.removeKey ? "Keep the key it has" : "None")
                        )
                        .onChange(of: form.key) { _, value in
                            if !value.isEmpty { form.removeKey = false }
                        }
                        if form.hasKey {
                            Toggle("Remove the saved key", isOn: $form.removeKey)
                                .toggleStyle(.checkbox)
                                .disabled(!form.key.isEmpty)
                        }
                    }
                } else {
                    TextField("Command", text: $form.command, prompt: Text("npx"))
                    TextField("Arguments", text: $form.arguments, prompt: Text("-y linear-mcp"))
                }
            } footer: {
                if form.isRemote {
                    if !signsIn {
                        Text("A key is kept in the Keychain, not in the settings file.")
                    } else if name != nil {
                        Text("Plug asks you to sign in after you add it.")
                    } else {
                        Text("You sign in to this server, so it has no key.")
                    }
                }
            }

            Section {
                TextField(
                    "Variables",
                    text: $form.settings,
                    prompt: Text("NAME=value, one per line"),
                    axis: .vertical
                )
                .lineLimit(2...5)
            } footer: {
                if let problem = form.settingsProblem {
                    InlineWarning(problem)
                } else {
                    Text("Values the server's instructions ask for, such as API_KEY. Keys are kept in the Keychain.")
                }
            }

            extra
        }
        .formStyle(.grouped)
    }
}

extension ServerFormFields where Extra == EmptyView {
    init(form: Binding<ServerForm>, name: Binding<String>? = nil) {
        self.init(form: form, name: name, extra: { EmptyView() })
    }
}

/// A small warning beside a field or a row in a sheet: the orange triangle
/// and a sentence. The words stay grey; only the symbol carries the colour.
struct InlineWarning: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Label {
            Text(text)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
        }
    }
}
