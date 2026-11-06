import Foundation
import PlugIPC

/// Whether Plug's menu bar icon comes back after something else closed the
/// app. The daemon does the reopening and reads one file to know whether it
/// may: `menu-bar-hidden` beside its socket means the owner wants the app
/// left closed.
struct MenuBarPresence {
    static let preferenceKey = "keepInMenuBar"
    static var standard: MenuBarPresence {
        MenuBarPresence(
            marker: PlugPaths.marker(PlugPaths.menuBarHiddenMarker),
            defaults: .standard
        )
    }

    let marker: URL
    let defaults: UserDefaults

    /// On unless the person turned it off.
    var keeps: Bool {
        get { defaults.object(forKey: Self.preferenceKey) as? Bool ?? true }
        nonmutating set {
            defaults.set(newValue, forKey: Self.preferenceKey)
            appDidOpen()
        }
    }

    /// The app is open, so an earlier quit no longer hides it.
    func appDidOpen() {
        if keeps { try? FileManager.default.removeItem(at: marker) } else { hide() }
    }

    /// The owner quit the app themselves; it stays closed until they open it.
    func ownerDidQuit() { hide() }

    private func hide() {
        try? FileManager.default.createDirectory(
            at: marker.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        try? Data().write(to: marker, options: .atomic)
    }
}
