import Foundation
import PlugIPC

/// `service-disabled` beside the daemon's socket: the owner turned Plug off.
/// The app writes it before stopping the daemon, so neither a relaunch of the
/// app nor reconciliation starts Plug again until the owner turns it on.
struct ServiceDisabledMarker: Sendable {
    let url: URL

    init(home: PlugHome) {
        url = home.marker(PlugPaths.serviceDisabledMarker)
    }

    var isSet: Bool { FileManager.default.fileExists(atPath: url.path) }

    func set() throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data().write(to: url, options: .atomic)
    }

    /// A marker that is already gone is no failure: a second copy of the
    /// app, or a person, may have removed it.
    func clear() throws {
        do { try FileManager.default.removeItem(at: url) } catch CocoaError.fileNoSuchFile {}
    }
}
