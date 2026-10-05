import AppKit
import PlugIPC
import SwiftUI
import UniformTypeIdentifiers

/// Pictures of the apps people already recognize.
///
/// A row that says "Claude Desktop" has to be read. A row carrying Claude's
/// own icon is recognized before it is read, which is the whole point of a
/// status app: the answer should land at a glance, without a sentence.
///
/// The real icon is used whenever the app can be found on this Mac. When it
/// cannot — the app is not installed, or it is a command line tool with no
/// icon at all — a symbol stands in, chosen so the row still says what kind of
/// thing it is.
enum AppIcons {
    /// Bundle identifiers for the apps Plug can be wired into. Best effort:
    /// a wrong or missing entry costs a symbol instead of an icon, never an
    /// error, and the name lookup below catches most of the rest.
    private static let bundleIdentifiers: [String: String] = [
        "claude-desktop": "com.anthropic.claudefordesktop",
        // Claude Code uses Claude's desktop identity when it has a GUI icon.
        "claude-code": "com.anthropic.claudefordesktop",
        // Codex CLI and the Codex desktop client share OpenAI's installed app
        // identity on macOS (currently shipped as ChatGPT.app).
        "codex": "com.openai.codex",
        "codex-cli": "com.openai.codex",
        "cursor": "com.todesktop.230313mzl4w4u92",
        "vscode": "com.microsoft.VSCode",
        "opencode": "ai.opencode.desktop",
        // Devin.app kept the bundle identifier of the Windsurf app it was.
        "devin": "com.exafunction.windsurf",
        // Grok Bot is a remote client, but its Mac app supplies the icon.
        "grok-bot": "com.anysphere.sand",
        "zed": "dev.zed.Zed",
        "antigravity": "com.google.antigravity",
        "junie": "com.jetbrains.junie",
        "goose": "com.block.goose",
        // Read off Homebrew's casks on 2026-10-04, not off an installed copy.
        "warp": "dev.warp.Warp-Stable",
        "kiro": "dev.kiro.desktop",
        "hermes": "com.nousresearch.hermes",
        // Remote clients whose Mac app supplies the icon. Perplexity's
        // identifier is unchecked; a miss costs a symbol.
        "gemini": "com.google.GeminiMacOS",
        "perplexity": "ai.perplexity.mac",
        "chatgpt": "com.openai.chat",
    ]

    /// Targets that are command line tools. They have no icon to show, and a
    /// terminal glyph says more about them than a generic app square would.
    private static let commandLineTargets: Set<String> = [
        "cline-cli", "gemini-cli", "grok-build", "copilot-cli", "pi",
        "goose", "opencode", "nanobot", "crush", "kimi-code", "qwen-code",
        "openclaw", "muse-code",
    ]

    /// Clients that live on someone else's servers and reach Plug over the
    /// public address. They have no link target, only a name and a picture.
    private static let remoteTargets: Set<String> = ["gemini", "perplexity", "le-chat", "muse"]

    /// Agents a person talks to by text message.
    private static let textAgentTargets: Set<String> = ["poke"]

    /// The symbol that stands in for an app with no icon on this Mac.
    ///
    /// Pure, so the choice can be tested without a filesystem.
    static func symbol(target: String, name: String = "") -> String {
        let key = target.lowercased()
        let text = "\(key) \(name.lowercased())"
        // Keep Claude variants recognizable even when Claude.app is not
        // installed. AppIcons.image uses the same installed icon when it is.
        if key == "claude-code" || key == "claude-desktop" || text.contains("claude") {
            return "sparkles"
        }
        // Codex has one visual identity across its CLI and desktop clients.
        // The installed Codex app supplies the official artwork; this is the
        // stable system fallback when that app is absent.
        if key == "codex" || key == "codex-cli" || text.contains("codex") {
            return "app"
        }
        // Goose is optional and often CLI-only. A bird is clearer than a
        // terminal glyph while still remaining a system-provided fallback.
        if key == "goose" || text.contains("goose") {
            return "bird"
        }
        // Amp is a command line tool too. Its name is too short to match on
        // inside another, so it is not in the set above.
        if commandLineTargets.contains(key) || key == "amp" || text.contains("cli") { return "terminal" }
        // An agent reached by text message has no app to show.
        if textAgentTargets.contains(key) { return "message" }
        if remoteTargets.contains(key) { return "globe" }
        if text.contains("code") || text.contains("cursor") || text.contains("zed") {
            return "chevron.left.forwardslash.chevron.right"
        }
        return "app"
    }

    /// The app's real icon, when this Mac has the app.
    @MainActor
    static func image(target: String, name: String, appPath: String? = nil) -> NSImage? {
        if let identifier = bundleIdentifiers[target.lowercased()],
           let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: identifier) {
            return NSWorkspace.shared.icon(forFile: url.path)
        }
        // A client Plug has no entry for, started by an app that is right here.
        if let appPath, FileManager.default.fileExists(atPath: appPath) {
            return NSWorkspace.shared.icon(forFile: appPath)
        }
        return installedIcon(named: name)
    }

    /// The icon of the app a server stands for, when this Mac has that app:
    /// a server named "slack" shows Slack's icon.
    @MainActor
    static func image(forServer name: String) -> NSImage? {
        installedIcon(named: appName(forServer: name) { installedApps[$0] != nil })
    }

    /// The app a command lives inside, when it lives inside one: a server
    /// started from `/Applications/Foo.app/Contents/MacOS/foo-mcp` is Foo.
    ///
    /// Pure, so the matching is testable.
    static func appBundle(containing command: String) -> String? {
        guard let end = command.range(of: ".app/") else { return nil }
        return String(command[..<end.lowerBound]) + ".app"
    }

    /// Server names that are not their app's name.
    private static let serverAliases: [String: String] = [
        "imessage": "messages",
        "github": "githubdesktop",
        "applenotes": "notes",
        "gdrive": "googledrive",
        "workspace": "googledrive",
        "googleworkspace": "googledrive",
        "gmail": "googledrive",
    ]

    /// The lookup key of the app a server stands for. Google's servers go
    /// by many names, one per product, and a Mac has an app for only a few
    /// of them, so the rest share Google Drive's icon.
    ///
    /// Pure, so the matching is testable.
    static func appName(forServer name: String, installed: (String) -> Bool = { _ in false }) -> String {
        let key = lookupKey(name)
        if let alias = serverAliases[key] { return alias }
        if key.hasPrefix("google"), !installed(key) { return "googledrive" }
        return key
    }

    /// A name with everything but its letters and digits removed, so
    /// "agent-admin" finds AgentAdmin.app.
    ///
    /// Pure, so the matching is testable.
    static func lookupKey(_ name: String) -> String {
        String(name.lowercased().unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) })
    }

    /// Where each installed app is, by lookup key. Read once: an app
    /// installed while Plug is open shows its icon after the next launch.
    @MainActor
    private static let installedApps: [String: String] = {
        var found: [String: String] = [:]
        let directories = ["/Applications", "\(NSHomeDirectory())/Applications", "/System/Applications"]
        for directory in directories {
            let names = (try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []
            for file in names where file.hasSuffix(".app") {
                let key = lookupKey(String(file.dropLast(4)))
                if found[key] == nil { found[key] = "\(directory)/\(file)" }
            }
        }
        return found
    }()

    @MainActor
    private static func installedIcon(named name: String) -> NSImage? {
        let key = lookupKey(name)
        guard !key.isEmpty, let path = installedApps[key] else { return nil }
        return NSWorkspace.shared.icon(forFile: path)
    }

    /// Match a live session's reported client type to a known target, so a
    /// session row can show the same icon the app row shows.
    ///
    /// Pure, so the matching rules are testable.
    static func target(forClientType clientType: String) -> String {
        let value = clientType
            .lowercased()
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
            .joined(separator: "-")
        let compact = value.replacingOccurrences(of: "-", with: "")
        if compact.contains("claudecode") { return "claude-code" }
        if compact.contains("claude") { return "claude-desktop" }
        if compact.contains("codex") {
            return "codex-cli"
        }
        if compact.contains("openai") || compact.contains("chatgpt") { return "chatgpt" }
        if compact.contains("devin") || compact.contains("cascade") ||
            compact.contains("windsurf") || compact.contains("codeium") {
            return "devin"
        }
        // Copilot runs in VS Code and in a terminal. The daemon's own
        // detection splits them the same way.
        if compact.contains("copilot") {
            return compact.contains("cli") ? "copilot-cli" : "vscode"
        }
        // The two xAI clients share a first word. The daemon's own
        // detection splits them the same way.
        if compact.contains("grok") {
            return compact.contains("bot") ? "grok-bot" : "grok-build"
        }
        // Gemini is a terminal client and a remote one.
        if compact.contains("gemini") {
            return compact.contains("cli") ? "gemini-cli" : "gemini"
        }
        if compact.contains("mistral") || compact.contains("lechat") { return "le-chat" }
        if compact.contains("poke") { return "poke" }
        // Meta has two: Muse, its agent, and Muse Code, in a terminal.
        if compact.contains("muse") {
            return compact.contains("code") ? "muse-code" : "muse"
        }
        for target in bundleIdentifiers.keys where value.contains(target) { return target }
        for target in commandLineTargets where value.contains(target) { return target }
        return value
    }

    /// The distinct apps behind a set of live sessions, first seen first, so
    /// a row of icons stays stable while sessions come and go.
    ///
    /// Pure, so the grouping is testable.
    static func distinctTargets(forClientTypes clientTypes: [String]) -> [String] {
        var seen = Set<String>()
        return clientTypes.compactMap {
            let target = target(forClientType: $0)
            return seen.insert(target).inserted ? target : nil
        }
    }

    /// Canonical product name for live client sessions. Unknown clients stay
    /// unknown; Plug must not turn an opaque client identifier into a guess.
    static func displayName(forTarget target: String) -> String? {
        switch target.lowercased() {
        case "claude-desktop": return "Claude Desktop"
        case "claude-code": return "Claude Code"
        case "codex", "codex-cli": return "Codex CLI"
        case "cursor": return "Cursor"
        case "devin": return "Devin"
        case "copilot-cli": return "GitHub Copilot CLI"
        case "grok-build": return "Grok Build"
        case "grok-bot": return "Grok Bot"
        case "opencode": return "OpenCode"
        case "goose": return "Goose"
        case "warp": return "Warp"
        case "kiro": return "Kiro"
        case "hermes": return "Hermes Agent"
        case "gemini": return "Gemini"
        case "perplexity": return "Perplexity"
        case "le-chat": return "Le Chat"
        case "muse": return "Muse"
        case "muse-code": return "Muse Code"
        case "poke": return "Poke"
        default: return nil
        }
    }
}

/// One app, shown as itself. An app with no icon on this Mac gets a tile
/// with its symbol or its first letter, so every row has a picture of the
/// same size and weight.
struct AppGlyph: View {
    let target: String
    let name: String
    /// The app bundle behind a client with no entry of its own.
    var appPath: String? = nil
    var size: CGFloat = 18

    var body: some View {
        Group {
            if let icon = IconStore.shared.image(forClient: target, name: name, appPath: appPath) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                let symbol = AppIcons.symbol(target: target, name: name)
                MonogramTile(
                    name: name.isEmpty ? target : name,
                    symbol: symbol == "app" ? nil : symbol,
                    size: size
                )
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// One server, shown as the best picture Plug has for it, and as a tile
/// with its first letter when it has none.
struct ServerGlyph: View {
    let name: String
    var size: CGFloat = 18
    /// The server's state as a dot on the corner of the icon, the way an
    /// app shows a contact's presence. Nil draws no dot.
    var status: Color?

    var body: some View {
        Group {
            if let icon = IconStore.shared.image(forServer: name) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
            } else {
                MonogramTile(name: name, size: size)
            }
        }
        .frame(width: size, height: size)
        .overlay(alignment: .bottomTrailing) {
            if let status {
                let dot = max(7, size * 0.32)
                Circle()
                    .fill(status)
                    .frame(width: dot, height: dot)
                    .overlay(Circle().stroke(.background, lineWidth: dot * 0.2))
                    .offset(x: dot * 0.15, y: dot * 0.15)
            }
        }
        .accessibilityHidden(true)
    }
}

/// The stand-in for an icon: a rounded tile carrying a symbol or the first
/// letter of a name. The color comes from the name, so it stays the same
/// from one launch to the next.
struct MonogramTile: View {
    let name: String
    /// A symbol that says what kind of thing this is, shown instead of the
    /// letter on a neutral tile.
    var symbol: String?
    var size: CGFloat = 18

    private nonisolated static let tints: [Color] = [.blue, .indigo, .purple, .pink, .orange, .teal, .green, .brown]

    /// Which tint a name gets.
    ///
    /// Pure, so it can be tested: the same name always gets the same tint.
    nonisolated static func tintIndex(for name: String) -> Int {
        let sum = name.lowercased().unicodeScalars.reduce(0) { ($0 &* 31 &+ Int($1.value)) % 1_000_003 }
        return sum % tints.count
    }

    nonisolated static func letter(for name: String) -> String {
        name.first { $0.isLetter || $0.isNumber }.map { String($0).uppercased() } ?? "?"
    }

    var body: some View {
        // App icons keep a margin inside their square; the tile keeps the
        // same one so the two sit level in a list.
        let side = size * 0.84
        RoundedRectangle(cornerRadius: side * 0.24, style: .continuous)
            .fill(symbol == nil ? AnyShapeStyle(Self.tints[Self.tintIndex(for: name)].gradient) : AnyShapeStyle(Color.gray.gradient))
            .frame(width: side, height: side)
            .overlay {
                if let symbol {
                    Image(systemName: symbol)
                        .font(.system(size: side * 0.5, weight: .semibold))
                        .foregroundStyle(.white)
                } else {
                    Text(Self.letter(for: name))
                        .font(.system(size: side * 0.58, weight: .semibold, design: .rounded))
                        .foregroundStyle(.white)
                }
            }
            .frame(width: size, height: size)
    }
}

// MARK: - Found icons

/// One server as the search for its icon sees it.
struct IconSource: Equatable, Sendable {
    let name: String
    /// Where a remote server is reached.
    var address: String?
    /// What starts a local server.
    var command: String?
    var args: [String] = []
    /// The web page the server gave for itself.
    var website: String?
    /// The icons the server gave for itself.
    var icons: [ServerIcon] = []
}

/// Every icon Plug found or was given, and the order they are trusted in.
///
/// A server's icon is, first to last: the one its owner chose, the one the
/// server offers for itself, the app on this Mac with its name, the app its
/// command lives in, and the icon of a web site: its own, then its
/// maker's, then the one its package names. A client's is the one its
/// owner chose, its app on this Mac, a logo that ships with Plug for the
/// command line clients that have no app, then its maker's web site.
///
/// A web site is asked only when nothing earlier answered and only over
/// HTTPS. Each one asked belongs to the server, its maker, or the registry
/// its package came from, so no icon service learns which servers are here.
@MainActor @Observable
final class IconStore {
    static let shared = IconStore()

    private(set) var chosen: [String: NSImage] = [:]
    private var advertised: [String: NSImage] = [:]
    private var commandApps: [String: NSImage] = [:]
    private var sites: [String: NSImage] = [:]
    private var clientSites: [String: NSImage] = [:]

    @ObservationIgnored private let chosenDirectory: URL
    @ObservationIgnored private let cacheDirectory: URL
    @ObservationIgnored private var signature = ""
    @ObservationIgnored private var search: Task<Void, Never>?
    /// What was already looked for since launch, found or not.
    @ObservationIgnored private var searched: Set<String> = []
    @ObservationIgnored private var clientsAsked: Set<String> = []

    /// A found icon is kept this long before its site is asked again.
    private static let keepFound: TimeInterval = 30 * 24 * 3600
    /// A site with no icon is asked again after this long.
    private static let keepMissing: TimeInterval = 24 * 3600

    init(chosenDirectory: URL? = nil, cacheDirectory: URL? = nil) {
        self.chosenDirectory = chosenDirectory
            ?? PlugIPCClient.defaultSocketURL.deletingLastPathComponent().appending(path: "icons")
        self.cacheDirectory = cacheDirectory
            ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appending(path: Bundle.main.bundleIdentifier ?? "com.cyberpapiii.plug")
            .appending(path: "icons")
        let files = (try? FileManager.default.contentsOfDirectory(atPath: self.chosenDirectory.path)) ?? []
        for file in files where file.hasSuffix(".png") {
            chosen[String(file.dropLast(4))] = NSImage(contentsOf: self.chosenDirectory.appending(path: file))
        }
    }

    // MARK: Reading

    func image(forServer name: String) -> NSImage? {
        chosen[Self.key(server: name)]
            ?? advertised[name]
            ?? AppIcons.image(forServer: name)
            ?? commandApps[name]
            ?? sites[name]
    }

    func image(forClient target: String, name: String, appPath: String? = nil) -> NSImage? {
        chosen[Self.key(client: target)]
            ?? AppIcons.image(target: target, name: name, appPath: appPath)
            ?? NSImage(named: "client-\(target.lowercased())")
            ?? clientSites[target]
            ?? findLater(client: target, name: name)
    }

    /// Ask a client's maker for its icon, once. Answers nil now; the icon
    /// shows when it arrives.
    private func findLater(client target: String, name: String) -> NSImage? {
        guard !clientsAsked.contains(target),
              let host = SiteIcon.brandHost(forName: name) ?? SiteIcon.brandHost(forName: target) else { return nil }
        clientsAsked.insert(target)
        Task { [weak self] in
            guard let self else { return }
            let key = "site-\(host)"
            switch self.kept(key) {
            case let .found(image): self.clientSites[target] = image
            case .missing: break
            case .unknown: self.clientSites[target] = self.keep(await SiteIcon.find(hosts: [host]), as: key)
            }
        }
        return nil
    }

    /// The name a chosen icon is kept under.
    nonisolated static func key(server name: String) -> String { "server-\(AppIcons.lookupKey(name))" }
    nonisolated static func key(client target: String) -> String { "client-\(AppIcons.lookupKey(target))" }

    // MARK: Choosing

    /// Ask for a picture and use it as the icon kept under `key`.
    func chooseIcon(for key: String) {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.image]
        panel.allowsMultipleSelection = false
        panel.message = "Choose a picture to use as the icon."
        panel.prompt = "Choose"
        guard panel.runModal() == .OK, let url = panel.url,
              let data = try? Data(contentsOf: url) else { return }
        setChosenIcon(data, for: key)
    }

    /// False when the data is not a picture.
    @discardableResult
    func setChosenIcon(_ data: Data, for key: String) -> Bool {
        guard let png = IconTile.png(from: data), let image = NSImage(data: png) else { return false }
        try? FileManager.default.createDirectory(at: chosenDirectory, withIntermediateDirectories: true)
        try? png.write(to: chosenDirectory.appending(path: "\(key).png"), options: .atomic)
        chosen[key] = image
        return true
    }

    func removeChosenIcon(for key: String) {
        try? FileManager.default.removeItem(at: chosenDirectory.appending(path: "\(key).png"))
        chosen[key] = nil
    }

    // MARK: Finding

    /// Look for icons whenever the set of servers, or their health, changes.
    func attach(to model: AppModel) {
        guard model.snapshotDidLoad == nil else { return }
        model.snapshotDidLoad = { [weak self, weak model] snapshot in
            guard let self, let model else { return }
            self.observe(snapshot, model: model)
        }
    }

    private func observe(_ snapshot: OperatorSnapshot, model: AppModel) {
        let configured = snapshot.configuredServers.map { "\($0.name):\($0.enabled)" }
        let running = snapshot.servers.map { "\($0.serverId)=\($0.health)" }
        let next = (configured + ["|"] + running).joined(separator: ",")
        guard next != signature else { return }
        signature = next
        search?.cancel()
        search = Task { [weak self, weak model] in
            guard let model else { return }
            let described = (try? await model.serverDescriptions()) ?? []
            var sources: [IconSource] = []
            for server in snapshot.configuredServers {
                let config = try? await model.serverConfig(name: server.name)
                let upstream = described.first { $0.serverId == server.name }?.upstream
                sources.append(IconSource(
                    name: server.name,
                    address: config?.url,
                    command: config?.command,
                    args: config?.args ?? [],
                    website: upstream?.websiteUrl,
                    icons: upstream?.icons ?? []
                ))
            }
            guard !Task.isCancelled else { return }
            await self?.load(sources)
        }
    }

    func load(_ sources: [IconSource]) async {
        var wanted: [(name: String, key: String, hosts: [String], package: SiteIcon.Package?)] = []
        for source in sources {
            if let command = source.command, let app = AppIcons.appBundle(containing: command),
               commandApps[source.name] == nil, FileManager.default.fileExists(atPath: app) {
                commandApps[source.name] = NSWorkspace.shared.icon(forFile: app)
            }
            if advertised[source.name] == nil, let icon = SiteIcon.best(of: source.icons) {
                if let data = SiteIcon.data(fromDataURI: icon.src) {
                    advertised[source.name] = IconTile.png(from: data).flatMap(NSImage.init(data:))
                } else if let url = URL(string: icon.src), url.scheme == "https", let host = url.host {
                    let key = "offered-\(AppIcons.lookupKey(host + url.path))"
                    switch kept(key) {
                    case let .found(image): advertised[source.name] = image
                    case .missing: break
                    case .unknown: advertised[source.name] = keep(await SiteIcon.image(at: url), as: key)
                    }
                }
            }
            guard image(forServer: source.name) == nil else { continue }
            var hosts = SiteIcon.hosts(server: source.address, website: source.website)
            if let brand = SiteIcon.brandHost(forName: source.name), !hosts.contains(brand) { hosts.append(brand) }
            let package = SiteIcon.package(command: source.command, args: source.args)
            if let first = hosts.first {
                wanted.append((source.name, "site-\(first)", hosts, package))
            } else if let package {
                wanted.append((source.name, "package-\(AppIcons.lookupKey(package.name))", hosts, package))
            }
        }
        // Sites answer at their own pace; ask them side by side.
        var asking: [(name: String, key: String, hosts: [String], package: SiteIcon.Package?)] = []
        for want in wanted {
            switch kept(want.key) {
            case let .found(image): sites[want.name] = image
            case .missing: break
            case .unknown: asking.append(want)
            }
        }
        let answers = await withTaskGroup(of: (Int, Data?).self) { group in
            for (index, want) in asking.enumerated() {
                let hosts = want.hosts
                let package = want.package
                group.addTask {
                    if let data = await SiteIcon.find(hosts: hosts) { return (index, data) }
                    guard let package else { return (index, nil) }
                    return (index, await SiteIcon.find(package: package))
                }
            }
            var answers: [Int: Data] = [:]
            for await (index, data) in group { answers[index] = data }
            return answers
        }
        for (index, want) in asking.enumerated() {
            if let image = keep(answers[index], as: want.key) { sites[want.name] = image }
        }
    }

    private enum Kept {
        case found(NSImage)
        /// Looked for not long ago, and not there.
        case missing
        case unknown
    }

    /// What is already known about the picture kept under `key`. Each key
    /// is unknown once per launch.
    private func kept(_ key: String) -> Kept {
        let file = cacheDirectory.appending(path: "\(key).png")
        if let age = Self.age(of: file), age < Self.keepFound, let image = NSImage(contentsOf: file) {
            return .found(image)
        }
        if let age = Self.age(of: cacheDirectory.appending(path: "\(key).none")), age < Self.keepMissing {
            return .missing
        }
        return searched.insert(key).inserted ? .unknown : .missing
    }

    /// Remember what a search for `key` came back with.
    private func keep(_ data: Data?, as key: String) -> NSImage? {
        let file = cacheDirectory.appending(path: "\(key).png")
        let missing = cacheDirectory.appending(path: "\(key).none")
        try? FileManager.default.createDirectory(at: cacheDirectory, withIntermediateDirectories: true)
        guard let data, let image = NSImage(data: data) else {
            try? Data().write(to: missing)
            // An icon past its time is still better than none.
            return NSImage(contentsOf: file)
        }
        try? data.write(to: file, options: .atomic)
        try? FileManager.default.removeItem(at: missing)
        return image
    }

    private static func age(of file: URL) -> TimeInterval? {
        let attributes = try? FileManager.default.attributesOfItem(atPath: file.path)
        return (attributes?[.modificationDate] as? Date).map { -$0.timeIntervalSinceNow }
    }
}

/// The two menu items that let a person pick an icon and take it back.
struct IconMenu: View {
    /// The name the chosen icon is kept under.
    let key: String

    var body: some View {
        Button("Choose Icon…") { IconStore.shared.chooseIcon(for: key) }
        if IconStore.shared.chosen[key] != nil {
            Button("Use Default Icon") { IconStore.shared.removeChosenIcon(for: key) }
        }
    }
}

/// Finding the icon a server offers, or the one its web site carries.
enum SiteIcon {
    private static let maxPage = 2_000_000
    private static let maxImage = 1_000_000

    /// The icon to use out of the ones a server offers: the largest.
    static func best(of icons: [ServerIcon]) -> ServerIcon? {
        func side(_ icon: ServerIcon) -> Int {
            (icon.sizes ?? []).map { size -> Int in
                if size.lowercased() == "any" { return 4096 }
                return Int(size.lowercased().split(separator: "x").first ?? "") ?? 0
            }.max() ?? 0
        }
        var best: ServerIcon?
        for icon in icons where best.map({ side(icon) > side($0) }) ?? true { best = icon }
        return best
    }

    /// The bytes inside a base64 `data:` address.
    static func data(fromDataURI source: String) -> Data? {
        guard source.lowercased().hasPrefix("data:"), let comma = source.firstIndex(of: ","),
              source[..<comma].lowercased().hasSuffix(";base64") else { return nil }
        return Data(base64Encoded: String(source[source.index(after: comma)...]))
    }

    /// The hosts to ask for an icon, most likely first: the server's own,
    /// the site above it, then the page the server named. HTTPS only, so a
    /// server on this Mac or on the local network is never asked.
    static func hosts(server: String?, website: String?) -> [String] {
        var found: [String] = []
        for address in [server, website] {
            guard let address, let url = URL(string: address), url.scheme?.lowercased() == "https",
                  let host = url.host?.lowercased(), !host.isEmpty else { continue }
            for candidate in [host, parent(of: host)].compactMap({ $0 }) where !found.contains(candidate) {
                found.append(candidate)
            }
        }
        return found
    }

    /// Makers' web sites, by the word their servers and clients are named
    /// with. A local command has no address of its own to ask, and its
    /// name is all there is to go on.
    private static let brands: [String: String] = [
        "airtable": "airtable.com", "amp": "ampcode.com", "amplitude": "amplitude.com", "anthropic": "anthropic.com",
        "asana": "asana.com", "atlassian": "atlassian.com", "aws": "aws.amazon.com",
        "box": "box.com", "brave": "brave.com", "bun": "bun.sh", "canva": "canva.com",
        "clickup": "clickup.com", "cloudflare": "cloudflare.com", "confluence": "atlassian.com",
        "context7": "context7.com", "datadog": "datadoghq.com", "discord": "discord.com",
        "docker": "docker.com", "dropbox": "dropbox.com", "elevenlabs": "elevenlabs.io",
        "exa": "exa.ai", "figma": "figma.com", "firecrawl": "firecrawl.dev",
        "gemini": "gemini.google.com", "github": "github.com", "gitlab": "gitlab.com",
        "grafana": "grafana.com", "homeassistant": "home-assistant.io", "hubspot": "hubspot.com",
        "huggingface": "huggingface.co", "intercom": "intercom.com", "jira": "atlassian.com",
        "kimi": "kimi.com", "lmstudio": "lmstudio.ai", "muse": "meta.ai", "openclaw": "openclaw.ai", "krisp": "krisp.ai", "kubernetes": "kubernetes.io",
        "linear": "linear.app", "miro": "miro.com", "mistral": "mistral.ai",
        "mixpanel": "mixpanel.com", "monday": "monday.com", "mongodb": "mongodb.com",
        "mysql": "mysql.com", "netlify": "netlify.com", "node": "nodejs.org",
        "notion": "notion.com", "obsidian": "obsidian.md", "openai": "openai.com",
        "oura": "ouraring.com", "pagerduty": "pagerduty.com", "paypal": "paypal.com",
        "perplexity": "perplexity.ai", "plaid": "plaid.com", "playwright": "playwright.dev",
        "postgres": "postgresql.org", "postgresql": "postgresql.org", "posthog": "posthog.com",
        "python": "python.org", "python3": "python.org", "qwen": "qwen.ai",
        "raycast": "raycast.com", "reddit": "reddit.com", "redis": "redis.io",
        "replicate": "replicate.com", "salesforce": "salesforce.com", "sentry": "sentry.io",
        "shopify": "shopify.com", "slack": "slack.com", "snowflake": "snowflake.com",
        "spotify": "spotify.com", "sqlite": "sqlite.org", "strava": "strava.com",
        "stripe": "stripe.com", "supabase": "supabase.com", "svelte": "svelte.dev",
        "tavily": "tavily.com", "telegram": "telegram.org", "todoist": "todoist.com",
        "trello": "trello.com", "twilio": "twilio.com", "vercel": "vercel.com",
        "xero": "xero.com", "youtube": "youtube.com", "zapier": "zapier.com",
        "zendesk": "zendesk.com", "zoom": "zoom.us",
    ]

    /// The maker's site for a name: the whole name, then each word of it.
    /// "oura" and "oura-mcp" both give Oura's.
    static func brandHost(forName name: String) -> String? {
        if let host = brands[AppIcons.lookupKey(name)] { return host }
        let words = name.lowercased().split(whereSeparator: { !$0.isLetter && !$0.isNumber })
        return words.lazy.compactMap { brands[String($0)] }.first
    }

    /// A package a registry can describe.
    struct Package: Equatable, Sendable {
        enum Registry: Sendable { case npm, pypi }
        let registry: Registry
        let name: String
    }

    /// The package a command runs, when it runs one straight from a
    /// registry: `npx -y @scope/thing@1.2` runs `@scope/thing`.
    static func package(command: String?, args: [String]) -> Package? {
        guard let tool = command?.split(separator: "/").last else { return nil }
        let registry: Package.Registry
        switch tool {
        case "npx", "bunx", "pnpx": registry = .npm
        case "uvx", "pipx": registry = .pypi
        default: return nil
        }
        guard var name = args.first(where: { !$0.hasPrefix("-") && $0 != "run" }) else { return nil }
        // Drop a version, keeping the "@" a scope starts with.
        if let at = name.lastIndex(of: "@"), at != name.startIndex { name = String(name[..<at]) }
        if registry == .pypi, let cut = name.firstIndex(where: { "=<>[".contains($0) }) { name = String(name[..<cut]) }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "@/._-"))
        guard !name.isEmpty, name.unicodeScalars.allSatisfy(allowed.contains) else { return nil }
        return Package(registry: registry, name: name)
    }

    /// Where to look for a package's icon, given the links its registry
    /// lists: its own site, and its owner's picture on GitHub.
    static func places(forPackageLinks links: [String]) -> (hosts: [String], picture: URL?) {
        var hosts: [String] = []
        var picture: URL?
        for link in links {
            let address = link.replacingOccurrences(of: "git+", with: "")
            guard let url = URL(string: address), let host = url.host?.lowercased() else { continue }
            if host == "github.com" || host == "www.github.com" {
                let owner = url.path.split(separator: "/").first.map(String.init) ?? ""
                if picture == nil, !owner.isEmpty { picture = URL(string: "https://github.com/\(owner).png") }
            } else if url.scheme == "https", !host.hasSuffix("npmjs.com"), !host.hasSuffix("pypi.org"),
                      !hosts.contains(host) {
                hosts.append(host)
            }
        }
        return (hosts, picture)
    }

    /// The icon of a package's site, or of its owner, as PNG data.
    static func find(package: Package) async -> Data? {
        let address = switch package.registry {
        case .npm: "https://registry.npmjs.org/\(package.name)/latest"
        case .pypi: "https://pypi.org/pypi/\(package.name)/json"
        }
        let session = makeSession()
        defer { session.invalidateAndCancel() }
        guard let url = URL(string: address), let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200, data.count <= maxPage,
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        var links: [String] = []
        let facts = (object["info"] as? [String: Any]) ?? object
        for key in ["homepage", "home_page"] {
            if let link = facts[key] as? String { links.append(link) }
        }
        if let repository = facts["repository"] as? [String: Any], let link = repository["url"] as? String {
            links.append(link)
        } else if let link = facts["repository"] as? String {
            links.append(link)
        }
        if let urls = facts["project_urls"] as? [String: Any] {
            links += urls.keys.sorted().compactMap { urls[$0] as? String }
        }
        let places = places(forPackageLinks: links)
        if let found = await find(hosts: places.hosts) { return found }
        guard let picture = places.picture else { return nil }
        return await image(at: picture, session: session)
    }

    /// The site one label up: `mcp.example.com` gives `example.com`. Nil
    /// for an address made of numbers and for a name with nothing above it.
    static func parent(of host: String) -> String? {
        let labels = host.split(separator: ".")
        guard labels.count >= 3, !host.contains(":"),
              labels.contains(where: { $0.contains(where: \.isLetter) }) else { return nil }
        return labels.dropFirst().joined(separator: ".")
    }

    /// The icons a page links to, best first, then the two addresses sites
    /// keep an icon at by habit. HTTPS only.
    static func candidates(inHTML html: String, base: URL) -> [URL] {
        var scored: [(score: Int, url: URL)] = []
        let range = NSRange(html.startIndex..., in: html)
        let tags = (try? NSRegularExpression(pattern: "<link\\b[^>]*>", options: .caseInsensitive))?
            .matches(in: html, range: range) ?? []
        for match in tags {
            guard let tagRange = Range(match.range, in: html) else { continue }
            let tag = String(html[tagRange])
            guard let rel = attribute("rel", in: tag)?.lowercased(), rel.contains("icon"), !rel.contains("mask"),
                  let href = attribute("href", in: tag),
                  let url = URL(string: href, relativeTo: base)?.absoluteURL else { continue }
            let side = attribute("sizes", in: tag).flatMap { Int($0.lowercased().split(separator: "x").first ?? "") } ?? 0
            scored.append(((rel.contains("apple-touch") ? 1000 : 0) + side, url))
        }
        var found = scored.enumerated()
            .sorted { $0.element.score != $1.element.score ? $0.element.score > $1.element.score : $0.offset < $1.offset }
            .map(\.element.url)
        for path in ["/apple-touch-icon.png", "/favicon.ico"] {
            if let url = URL(string: path, relativeTo: base)?.absoluteURL { found.append(url) }
        }
        var seen = Set<URL>()
        return Array(found.filter { $0.scheme == "https" && seen.insert($0).inserted }.prefix(6))
    }

    private static func attribute(_ name: String, in tag: String) -> String? {
        let pattern = "\\b\(name)\\s*=\\s*(?:\"([^\"]*)\"|'([^']*)'|([^\\s>]+))"
        guard let expression = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive),
              let match = expression.firstMatch(in: tag, range: NSRange(tag.startIndex..., in: tag)) else { return nil }
        for group in 1...3 {
            if let range = Range(match.range(at: group), in: tag) { return String(tag[range]) }
        }
        return nil
    }

    /// The first icon any of `hosts` gives up, as PNG data.
    static func find(hosts: [String]) async -> Data? {
        let session = makeSession()
        defer { session.invalidateAndCancel() }
        for host in hosts {
            guard let root = URL(string: "https://\(host)/") else { continue }
            var html = ""
            var base = root
            if let (data, response) = try? await session.data(from: root), data.count <= maxPage,
               (response as? HTTPURLResponse)?.statusCode == 200 {
                html = String(decoding: data, as: UTF8.self)
                base = response.url ?? root
            }
            for url in candidates(inHTML: html, base: base) {
                if let data = await image(at: url, session: session) { return data }
            }
        }
        return nil
    }

    /// The picture at `url`, as PNG data.
    static func image(at url: URL) async -> Data? {
        let session = makeSession()
        defer { session.invalidateAndCancel() }
        return await image(at: url, session: session)
    }

    private static func image(at url: URL, session: URLSession) async -> Data? {
        guard url.scheme == "https", let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200, data.count <= maxImage else { return nil }
        return IconTile.png(from: data)
    }

    /// No cookies, no cache, no credentials: the request says nothing
    /// about this Mac beyond its address.
    private static func makeSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 8
        configuration.timeoutIntervalForResource = 15
        configuration.httpShouldSetCookies = false
        configuration.httpCookieAcceptPolicy = .never
        configuration.urlCache = nil
        configuration.httpAdditionalHeaders = ["User-Agent": "Mozilla/5.0 (Macintosh) Plug"]
        return URLSession(configuration: configuration)
    }
}

/// Any picture, made to sit beside app icons: the same size, the same
/// rounded shape, and a tile behind a bare mark so it shows on any
/// background.
enum IconTile {
    private static let side = 192

    /// Nil when the data is not a picture or is too small to show.
    static func png(from data: Data) -> Data? {
        guard let image = NSImage(data: data) else { return nil }
        var proposed = CGRect(x: 0, y: 0, width: side, height: side)
        guard let whole = image.cgImage(forProposedRect: &proposed, context: nil, hints: nil),
              whole.width >= 32, whole.height >= 32 else { return nil }
        // A picture often carries an empty margin of its own. Measure and
        // draw what is inside it, or the margin is added twice.
        guard let box = shape(of: whole)?.box,
              let source = whole.cropping(to: box),
              let shape = shape(of: source) else { return nil }

        guard let context = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8,
            bytesPerRow: side * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        context.interpolationQuality = .high
        let canvas = CGRect(x: 0, y: 0, width: side, height: side)
        // App icons keep a margin inside their square; so does this.
        let tile = canvas.insetBy(dx: canvas.width * 0.08, dy: canvas.width * 0.08)
        if shape.cornersClear, shape.clearShare < 0.3 {
            // Already shaped like an icon. Leave its shape alone.
            context.draw(source, in: fit(source, in: tile))
        } else {
            let radius = tile.width * 0.24
            context.addPath(CGPath(roundedRect: tile, cornerWidth: radius, cornerHeight: radius, transform: nil))
            context.clip()
            let bare = shape.clearShare >= 0.3 || source.width != source.height
            let shade: CGFloat = bare && shape.isLight ? 0.11 : 1
            context.setFillColor(CGColor(red: shade, green: shade, blue: shade, alpha: 1))
            context.fill(tile)
            let margin = bare ? tile.width * 0.16 : 0
            context.draw(source, in: fit(source, in: tile.insetBy(dx: margin, dy: margin)))
        }
        guard let result = context.makeImage() else { return nil }
        return NSBitmapImageRep(cgImage: result).representation(using: .png, properties: [:])
    }

    /// The largest rectangle of the picture's proportions inside `rect`.
    private static func fit(_ image: CGImage, in rect: CGRect) -> CGRect {
        let scale = min(rect.width / CGFloat(image.width), rect.height / CGFloat(image.height))
        let width = CGFloat(image.width) * scale, height = CGFloat(image.height) * scale
        return CGRect(x: rect.midX - width / 2, y: rect.midY - height / 2, width: width, height: height)
    }

    private struct Shape {
        /// The part of the picture that is not empty, in its own pixels.
        let box: CGRect
        /// How much of the picture is empty.
        let clearShare: Double
        let isLight: Bool
        let cornersClear: Bool
    }

    /// What a small copy of the picture says about it. Nil for a picture
    /// with nothing in it.
    private static func shape(of image: CGImage) -> Shape? {
        let probe = 64
        guard let measure = CGContext(
            data: nil, width: probe, height: probe, bitsPerComponent: 8,
            bytesPerRow: probe * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        measure.draw(image, in: CGRect(x: 0, y: 0, width: probe, height: probe))
        guard let pixels = measure.data?.assumingMemoryBound(to: UInt8.self) else { return nil }
        func alpha(_ x: Int, _ y: Int) -> Int { Int(pixels[(y * probe + x) * 4 + 3]) }
        var clear = 0
        var light = 0.0
        var left = probe, right = -1, top = probe, bottom = -1
        for y in 0..<probe {
            for x in 0..<probe {
                let index = y * probe + x
                let a = Double(pixels[index * 4 + 3]) / 255
                guard a >= 0.5 else { clear += 1; continue }
                left = min(left, x); right = max(right, x); top = min(top, y); bottom = max(bottom, y)
                let red = Double(pixels[index * 4]), green = Double(pixels[index * 4 + 1]), blue = Double(pixels[index * 4 + 2])
                light += (0.299 * red + 0.587 * green + 0.114 * blue) / 255 / a
            }
        }
        let solid = probe * probe - clear
        guard solid > 0 else { return nil }
        let last = probe - 1
        // The bitmap's first row is the picture's top row, as in a CGImage.
        let across = CGFloat(image.width) / CGFloat(probe), down = CGFloat(image.height) / CGFloat(probe)
        let box = CGRect(
            x: CGFloat(left) * across, y: CGFloat(top) * down,
            width: CGFloat(right - left + 1) * across, height: CGFloat(bottom - top + 1) * down
        ).integral.intersection(CGRect(x: 0, y: 0, width: image.width, height: image.height))
        return Shape(
            box: box,
            clearShare: Double(clear) / Double(probe * probe),
            isLight: light / Double(solid) > 0.8,
            cornersClear: [alpha(0, 0), alpha(last, 0), alpha(0, last), alpha(last, last)].allSatisfy { $0 < 128 }
        )
    }
}
