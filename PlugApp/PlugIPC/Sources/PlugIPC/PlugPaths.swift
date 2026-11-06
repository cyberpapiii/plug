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

    public static var directoryURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: directory, directoryHint: .isDirectory)
    }

    public static var logs: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: logDirectory, directoryHint: .isDirectory)
    }

    public static var socketURL: URL { directoryURL.appending(path: socketFile) }

    public static var tokenURL: URL { directoryURL.appending(path: tokenFile) }

    /// A marker file, which sits beside the socket.
    public static func marker(_ name: String) -> URL {
        socketURL.deletingLastPathComponent().appending(path: name)
    }
}
