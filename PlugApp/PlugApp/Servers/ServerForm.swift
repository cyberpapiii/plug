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

    var isComplete: Bool {
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

/// The form itself, shared by Add Server and Edit.
struct ServerFormFields: View {
    @Binding var form: ServerForm

    var body: some View {
        VStack(alignment: .leading, spacing: Metric.snug) {
            // An API is described by a document; it has no "where".
            if !form.isAPI {
                Picker("Where it runs", selection: $form.isRemote) {
                    Label("On this Mac", systemImage: "desktopcomputer").tag(false)
                    Label("Over the network", systemImage: "globe").tag(true)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }

            Form {
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
                        SecureField(
                            "Key",
                            text: $form.key,
                            prompt: Text(form.hasKey ? "Keep the key it has" : "None")
                        )
                        .onChange(of: form.key) { _, value in
                            if !value.isEmpty { form.removeKey = false }
                        }
                        if form.hasKey {
                            Toggle("Remove the key", isOn: $form.removeKey)
                                .disabled(!form.key.isEmpty)
                        }
                    } else {
                        TextField("Command", text: $form.command, prompt: Text("npx"))
                        TextField("Arguments", text: $form.arguments, prompt: Text("-y linear-mcp"))
                    }
                } footer: {
                    if form.isRemote {
                        Text("A key is kept in the Keychain, not in the settings file. Leave it empty for a server you sign in to.")
                    }
                }

                Section {
                    TextField(
                        "Settings",
                        text: $form.settings,
                        prompt: Text("NAME=value, one per line"),
                        axis: .vertical
                    )
                    .lineLimit(2...5)
                } footer: {
                    Text("Values the server's instructions ask for, such as API_KEY. One whose name says it is a key is kept in the Keychain.")
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .frame(height: 280)
        }
    }
}
