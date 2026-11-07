import Foundation

/// Where Plug keeps its files, and the names the app and the daemon agree
/// on. The daemon's copy is `plug-core/src/paths.rs`; a test holds both
/// to `testdata/paths.json`.
public enum PlugPaths {
    /// Under the home folder: the config, the socket, and Plug's state.
    public static let directory = "Library/Application Support/plug"
    /// Under the home folder: the daemon's logs and the app's own.
    public static let logDirectory = "Library/Logs/plug"
    public static let socketFile = "plug.sock"
    public static let tokenFile = "plug.token"
    /// Beside the socket: the owner turned Plug off, so nothing starts it.
    public static let serviceDisabledMarker = "service-disabled"
    /// Beside the socket: the owner hid the menu bar icon, so the daemon does
    /// not reopen a closed app.
    public static let menuBarHiddenMarker = "menu-bar-hidden"

    /// The launchd job the app registers for the daemon.
    public static let daemonLabel = "com.plug.daemon"
    public static let appBundleID = "com.cyberpapiii.plug"
    /// The Developer ID team that signs Plug.app.
    public static let teamID = "HJF7LN64XX"
    /// Where in Plug.app the `plug` binary is.
    public static let bundledBinary = "Contents/Resources/plug"
    /// What launchd runs the bundled binary with.
    public static let daemonArguments = ["serve", "--daemon"]
    /// Under the home folder: the `plug` command Plug.app links to its binary.
    public static let commandLink = ".local/bin/plug"

    public static var directoryURL: URL { PlugHome.standard.directory }

    public static var logs: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: logDirectory, directoryHint: .isDirectory)
    }

    public static var socketURL: URL { PlugHome.standard.socketURL }

    public static var tokenURL: URL { PlugHome.standard.tokenURL }

    /// A marker file, which sits beside the socket.
    public static func marker(_ name: String) -> URL { PlugHome.standard.marker(name) }
}

/// One folder holding a daemon's socket, token and markers: the owner's real
/// one, or a test's own, so a test never reads or writes the owner's files.
public struct PlugHome: Sendable, Equatable {
    public let directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    public static var standard: PlugHome {
        PlugHome(
            directory: FileManager.default.homeDirectoryForCurrentUser
                .appending(path: PlugPaths.directory, directoryHint: .isDirectory)
        )
    }

    public var socketURL: URL { directory.appending(path: PlugPaths.socketFile) }

    public var tokenURL: URL { directory.appending(path: PlugPaths.tokenFile) }

    /// A marker file, which sits beside the socket.
    public func marker(_ name: String) -> URL {
        socketURL.deletingLastPathComponent().appending(path: name)
    }
}
