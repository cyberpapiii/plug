import AppKit
import PlugIPC
import SwiftUI

/// What the Add a Client sheet offers, apart from how it is drawn.
enum AddClient {
    /// Which client is being added.
    enum Choice: Hashable {
        /// One Plug knows how to set up, by its link target.
        case app(String)
        /// One that lives on someone else's servers, by name.
        case hosted(String)
        /// Anything else.
        case other
    }

    /// How the client reaches Plug.
    enum Way: String, CaseIterable, Identifiable {
        case onThisMac = "On This Mac"
        case network = "Over the Network"

        var id: String { rawValue }

        var caption: String {
            switch self {
            case .onThisMac: "The client starts Plug's own command. Nothing to sign in to. Only for a client on this Mac."
            case .network: "The client uses Plug's address, signs in, and you approve it. Works for a client anywhere."
            }
        }
    }

    /// What the sheet does once a client and a way are chosen.
    enum Step: Equatable {
        /// Plug writes itself into the client's settings.
        case write(target: String)
        /// The client already reaches Plug this way.
        case done(target: String)
        /// The person pastes Plug's command into the client.
        case command
        /// The person pastes Plug's address into the client.
        case address
        /// The client is elsewhere and Plug has no address it can reach.
        case unreachable
    }

    /// Clients that run on someone else's servers.
    static let hosted = ["ChatGPT", "Claude on the web or phone"]

    /// The clients found on this Mac, by name.
    static func found(_ apps: [LinkableApp]) -> [LinkableApp] { sorted(apps.filter(\.detected)) }

    /// The clients Plug knows and did not find here.
    static func notFound(_ apps: [LinkableApp]) -> [LinkableApp] { sorted(apps.filter { !$0.detected }) }

    private static func sorted(_ apps: [LinkableApp]) -> [LinkableApp] {
        apps.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    /// The first client on this Mac that does not use Plug yet, else the
    /// one that can be anything.
    static func firstChoice(_ apps: [LinkableApp]) -> Choice {
        found(apps).first { !$0.linked }.map { .app($0.target) } ?? .other
    }

    /// A hosted client can only come in over the network.
    static func ways(for choice: Choice) -> [Way] {
        if case .hosted = choice { return [.network] }
        return Way.allCases
    }

    static func step(_ choice: Choice, way: Way, apps: [LinkableApp], address: String?) -> Step {
        switch choice {
        case let .app(target):
            guard let app = apps.first(where: { $0.target == target }), app.detected || app.linked else {
                // Plug will not write settings for a client it cannot find.
                return way == .onThisMac ? .command : .address
            }
            let linkedWay: Way? = app.linked ? (app.transport?.lowercased() == "http" ? .network : .onThisMac) : nil
            return linkedWay == way ? .done(target: target) : .write(target: target)
        case .hosted:
            return address.map(reachesBeyondThisMac) == true ? .address : .unreachable
        case .other:
            return way == .onThisMac ? .command : .address
        }
    }

    /// True when the address reaches Plug from outside this Mac.
    static func reachesBeyondThisMac(_ address: String) -> Bool {
        guard let host = URL(string: address)?.host()?.lowercased() else { return false }
        return !["localhost", "127.0.0.1", "::1", "[::1]"].contains(host)
    }

    /// What to do with the address.
    static func addressAdvice(_ address: String, name: String?) -> String {
        let place = name.map { "In \($0)" } ?? "In the client"
        let reach = reachesBeyondThisMac(address) ? "" : " This address works only on this Mac."
        return "\(place), add a connector or MCP server and paste this address. Plug then opens a page asking you to approve it.\(reach)"
    }

    /// What the key of a client the owner made up starts with. The daemon
    /// knows the same word.
    static let customPrefix = "custom:"

    /// The target a client the owner named connects as, which is also the
    /// key its name is stored under. Nil when the name has nothing to make
    /// one from. The daemon accepts lowercase letters, digits and hyphens.
    static func customTarget(named name: String) -> String? {
        let slug = name.lowercased().unicodeScalars
            .map { ("a"..."z").contains($0) || ("0"..."9").contains($0) ? String($0) : "-" }
            .joined()
            .split(separator: "-", omittingEmptySubsequences: true)
            .joined(separator: "-")
            .prefix(40)
        return slug.isEmpty ? nil : customPrefix + slug
    }

    /// What follows Plug's command. A target makes the client its own row.
    static func arguments(target: String?) -> [String] {
        target.map { ["connect", "--client", $0] } ?? ["connect"]
    }

    /// The settings entry most clients take, for pasting whole.
    static func settingsEntry(command: String, target: String? = nil) -> String {
        entry(["command": command, "args": arguments(target: target)])
    }

    static func settingsEntry(address: String) -> String {
        entry(["url": address])
    }

    private static func entry(_ plug: [String: Any]) -> String {
        let data = try? JSONSerialization.data(
            withJSONObject: ["mcpServers": ["plug": plug]],
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        )
        return data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
}

/// How a client that lost its sign-in gets it back. Only the client can start
/// one, so the app says where.
enum ClientSignIn {
    /// The command that signs a client in to Plug, for a client that has
    /// one. Plug can run it; every other client signs in from inside itself.
    static func command(target: String) -> String? {
        target == "codex-cli" ? "codex mcp login plug" : nil
    }

    static func steps(target: String, name: String) -> String {
        switch target {
        case "codex-cli": "In Terminal, run codex mcp login plug."
        case "claude-code": "In Claude Code, type /mcp, choose plug, then Authenticate."
        default: "Open \(name) and use Plug from there."
        }
    }

    static func about(target: String, name: String) -> String {
        "Its sign-in ended, so it cannot use Plug until it signs in again. "
            + (command(target: target) == nil ? steps(target: target, name: name) : "Press Sign In.")
            + " Plug then asks you to approve it in your browser. Turn this off to remove it instead."
    }
}

/// Adding a client is two choices, which client and how it connects, and
/// then one button or one thing to paste.
struct AddClientView: View {
    let model: AppModel
    @State private var choice: AddClient.Choice?
    @State private var way = AddClient.Way.onThisMac
    @State private var copied: String?
    /// What the owner calls a client Plug has no entry for.
    @State private var name = ""
    @State private var failure: String?

    private var apps: [LinkableApp] { model.connectableApps }
    private var address: String? { model.snapshot.clientAddress }
    private var chosen: AddClient.Choice { choice ?? AddClient.firstChoice(apps) }
    private var ways: [AddClient.Way] { AddClient.ways(for: chosen) }
    private var chosenWay: AddClient.Way { ways.contains(way) ? way : ways[0] }
    private var command: String {
        BundledPlug.executable?.path ?? "/Applications/Plug.app/Contents/Resources/plug"
    }

    private var chosenName: String? {
        switch chosen {
        case let .app(target): apps.first { $0.target == target }?.name
        case let .hosted(name): name
        case .other: nil
        }
    }

    var body: some View {
        SheetFrame(
            title: "Add a Client",
            subtitle: "A client is an app that uses your servers, such as Claude or Cursor.",
            failure: failure
        ) {
            Form {
                Section {
                    Picker("Client", selection: Binding(get: { chosen }, set: { choice = $0; copied = nil })) {
                        Section("On This Mac") {
                            ForEach(AddClient.found(apps)) { app in
                                Text(app.linked ? "\(app.name) (uses Plug)" : app.name)
                                    .tag(AddClient.Choice.app(app.target))
                            }
                        }
                        Section("On the Web") {
                            ForEach(AddClient.hosted, id: \.self) { Text($0).tag(AddClient.Choice.hosted($0)) }
                        }
                        Section("Not Found on This Mac") {
                            ForEach(AddClient.notFound(apps)) { app in
                                Text(app.name).tag(AddClient.Choice.app(app.target))
                            }
                        }
                        Section {
                            Text("Another Client").tag(AddClient.Choice.other)
                        }
                    }
                    if namesIt {
                        TextField("Name", text: $name, prompt: Text("What you call it"))
                            .onChange(of: name) { copied = nil }
                        if let customTarget {
                            LabeledContent("Icon") {
                                HStack(spacing: Metric.snug) {
                                    AppGlyph(target: customTarget, name: name, size: 22)
                                    Button("Choose…") {
                                        IconStore.shared.chooseIcon(for: IconStore.key(client: customTarget))
                                    }
                                }
                            }
                        }
                    }
                    if ways.count > 1 {
                        VStack(alignment: .leading, spacing: Metric.rowGap) {
                            Picker("Connects", selection: $way) {
                                ForEach(ways) { Text($0.rawValue).tag($0) }
                            }
                            .pickerStyle(.segmented)
                            caption(chosenWay.caption)
                        }
                    }
                }
                Section { step }
            }
            .formStyle(.grouped)
            .fixedSize(horizontal: false, vertical: true)
        }
        .task { await model.loadConnectableApps() }
    }

    @ViewBuilder private var step: some View {
        switch AddClient.step(chosen, way: chosenWay, apps: apps, address: address) {
        case let .write(target):
            let name = chosenName ?? target
            HStack(spacing: Metric.snug) {
                AppGlyph(target: target, name: name, size: 22)
                caption("Plug adds itself to \(name)'s settings.")
                Spacer(minLength: 0)
                if model.busyApps.contains(target) {
                    ProgressView().controlSize(.small)
                } else {
                    Button("Add to \(name)") {
                        Task { await model.setAppLinked(target, true, overNetwork: chosenWay == .network) }
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
        case let .done(target):
            let name = chosenName ?? target
            Label {
                Text(chosenWay == .network
                    ? "\(name) uses Plug. Restart it, then sign in: \(ClientSignIn.steps(target: target, name: name))"
                    : "\(name) uses Plug. Restart it to see your servers.")
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            }
        case .command:
            paste(
                label: "Command",
                value: ([command] + AddClient.arguments(target: customTarget)).joined(separator: " "),
                entry: AddClient.settingsEntry(command: command, target: customTarget)
            )
            caption("\(notFoundNote)In \(chosenName ?? "the client")'s MCP settings, add a server that runs this command. \(namingNote)")
        case .address:
            if let address {
                paste(label: "Address", value: address, entry: AddClient.settingsEntry(address: address))
                caption("\(notFoundNote)\(AddClient.addressAdvice(address, name: chosenName)) \(namingNote)")
            } else {
                caption("Plug is not running, so it has no address to give. Start Plug and open this again.")
            }
        case .unreachable:
            caption("Plug has no address outside this Mac yet, so \(chosenName ?? "this client") cannot reach it. Set up remote access first.")
        }
    }

    /// Said for a client Plug knows and cannot find here.
    private var notFoundNote: String {
        if case .app = chosen, let chosenName { return "Plug cannot find \(chosenName) on this Mac, so it cannot set it up for you. " }
        return ""
    }

    /// A client Plug has no entry for is named here when it runs Plug's
    /// command, which can carry the name. An address cannot.
    private var namesIt: Bool { chosen == .other && chosenWay == .onThisMac }

    private var customTarget: String? { namesIt ? AddClient.customTarget(named: name) : nil }

    private var namingNote: String {
        guard chosen == .other else { return "" }
        return customTarget == nil
            ? "It shows in Clients the first time it connects, where you can name it and choose its icon."
            : "It shows in Clients under that name and icon the first time it connects."
    }

    /// The name is stored when the command is copied, so the client has it
    /// the first time it connects.
    private func keepName() {
        guard let customTarget else { return }
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        failure = nil
        Task {
            do {
                try await model.performOperation { .renameClient(authToken: $0, key: customTarget, name: name) }
            } catch {
                failure = "Plug could not keep the name: \(error.localizedDescription)"
            }
        }
    }

    private func paste(label: String, value: String, entry: String) -> some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            LabeledContent(label) {
                Text(value)
                    .font(.body.monospaced())
                    .textSelection(.enabled)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
            HStack(spacing: Metric.snug) {
                Spacer(minLength: 0)
                copyButton("Copy", value)
                copyButton("Copy as Settings", entry)
            }
        }
    }

    private func copyButton(_ title: String, _ text: String) -> some View {
        Button(copied == title ? "Copied" : title) {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            copied = title
            keepName()
        }
    }

    private func caption(_ text: String) -> some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}
