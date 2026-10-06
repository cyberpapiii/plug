import AppKit
import PlugIPC

/// Where a server's own settings are, as far as Plug can tell from what it
/// already holds: the command and arguments that start the server, and the
/// page the server gave for itself. Nothing is searched for.
enum ServerSettingsPlace: Equatable {
    /// The app the server's command runs from.
    case app(path: String)
    /// A file or folder named in the server's arguments.
    case file(path: String)
    /// The page the server gave for itself.
    case page(URL)
    /// Nowhere but Plug's own form.
    case plug

    static func find(
        config: ServerConfig,
        website: String?,
        home: String = NSHomeDirectory(),
        exists: (String) -> Bool = { FileManager.default.fileExists(atPath: $0) }
    ) -> ServerSettingsPlace {
        let command = config.command.map { paths(in: $0, home: home) } ?? []
        let arguments = config.args.flatMap { paths(in: $0, home: home) }
        for path in command + arguments {
            if let app = AppIcons.appBundle(containing: path), exists(app) { return .app(path: app) }
        }
        if let file = arguments.first(where: exists) { return .file(path: file) }
        if let website, let url = URL(string: website), url.scheme == "https", url.host() != nil {
            return .page(url)
        }
        return .plug
    }

    /// The absolute paths one word of a command line names: the word itself,
    /// or what follows the equals sign of an option.
    private static func paths(in word: String, home: String) -> [String] {
        let value = word.hasPrefix("-") ? word.split(separator: "=", maxSplits: 1).dropFirst().first.map(String.init) : word
        guard var path = value else { return [] }
        if path.hasPrefix("~/") { path = home + path.dropFirst() }
        return path.hasPrefix("/") && path.count > 1 ? [path] : []
    }

    /// Where, in a few words.
    func label(home: String = NSHomeDirectory()) -> String {
        switch self {
        case let .app(path): "In \(Self.appName(path))"
        case let .file(path): path.hasPrefix(home + "/") ? "~" + path.dropFirst(home.count) : path
        case let .page(url): url.host() ?? url.absoluteString
        case .plug: "In Plug"
        }
    }

    /// What the button that goes there says.
    var actionTitle: String {
        switch self {
        case let .app(path): "Open \(Self.appName(path))"
        case .file: "Show in Finder"
        case .page: "Open Page"
        case .plug: "Edit…"
        }
    }

    private static func appName(_ path: String) -> String {
        URL(fileURLWithPath: path).deletingPathExtension().lastPathComponent
    }

    /// Go there. Plug's own form is opened by the caller.
    @MainActor
    func open() {
        switch self {
        case let .app(path): NSWorkspace.shared.open(URL(fileURLWithPath: path))
        case let .file(path): NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
        case let .page(url): NSWorkspace.shared.open(url)
        case .plug: break
        }
    }
}
