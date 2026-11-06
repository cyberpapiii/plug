import Foundation

/// The `plug` binary shipped inside the app bundle.
///
/// Services resolve the executable through here rather than repeating the
/// bundle lookup, so the resource name is written once. Callers still take the
/// URL as an injectable initializer default, which is what lets tests point a
/// service at a stub binary.
enum BundledPlug {
    static var executable: URL? {
        Bundle.main.url(forResource: "plug", withExtension: nil)
    }
}

enum PlugPaths {
    /// Where the daemon writes its logs, spelled as the daemon spells it
    /// (`plug/src/daemon/paths.rs`). The app's own log goes beside them.
    static var logs: URL {
        URL.homeDirectory.appending(path: "Library/Logs/plug", directoryHint: .isDirectory)
    }
}

extension URL {
    /// The same file path with symlinks resolved, for comparing two URLs that
    /// may reach the same file by different routes.
    ///
    /// Standardizing on both sides of `resolvingSymlinksInPath()` is deliberate:
    /// the first pass removes `..` and `.` components that would otherwise
    /// defeat resolution, and the second normalizes what resolution returns.
    var resolvedStandardized: URL {
        standardizedFileURL.resolvingSymlinksInPath().standardizedFileURL
    }
}
