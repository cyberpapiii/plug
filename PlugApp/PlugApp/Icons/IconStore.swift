import AppKit
import Observation
import PlugIPC
import UniformTypeIdentifiers

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
        // Beside the chosen icons, not under Caches: a disk cleaner empties
        // Caches, and every icon was then looked for again the next day.
        self.cacheDirectory = cacheDirectory ?? self.chosenDirectory.appending(path: "found")
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
    /// `origins` reads what every server said about itself, and the configs
    /// of the servers it is given: only those with no icon yet, since a
    /// config read is one daemon request per server.
    func observe(
        _ snapshot: OperatorSnapshot,
        origins: @escaping @MainActor ([String]) async -> [String: ServerOrigin]
    ) {
        let configured = snapshot.configuredServers.map { "\($0.name):\($0.enabled)" }
        let running = snapshot.servers.map { "\($0.serverId)=\($0.health)" }
        let next = (configured + ["|"] + running).joined(separator: ",")
        guard next != signature else { return }
        signature = next
        search?.cancel()
        let names = snapshot.configuredServers.map(\.name)
        let unfound = names.filter { image(forServer: $0) == nil }
        search = Task { [weak self] in
            let found = await origins(unfound)
            guard !Task.isCancelled, let self else { return }
            await self.load(names.map { name in
                let origin = found[name]
                return IconSource(
                    name: name,
                    address: origin?.config?.url,
                    command: origin?.config?.command,
                    args: origin?.config?.args ?? [],
                    website: origin?.upstream?.websiteUrl,
                    icons: origin?.upstream?.icons ?? []
                )
            })
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
                    case .unknown:
                        let data = await SiteIcon.image(at: url)
                        guard !stopped(asking: [key]) else { return }
                        advertised[source.name] = keep(data, as: key)
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
        guard !stopped(asking: asking.map(\.key)) else { return }
        for (index, want) in asking.enumerated() {
            if let image = keep(answers[index], as: want.key) { sites[want.name] = image }
        }
    }

    /// Whether this search was cancelled, as it is when the servers change
    /// while it runs. A cancelled request comes back empty, which is not a
    /// site without an icon: nothing is remembered and `keys` are asked again.
    private func stopped(asking keys: [String]) -> Bool {
        guard Task.isCancelled else { return false }
        searched.subtract(keys)
        return true
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
