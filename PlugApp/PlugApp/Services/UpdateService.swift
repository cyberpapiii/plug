import Observation
import Sparkle

@MainActor @Observable
final class UpdateService {
    static let shared = UpdateService()

    private let controller = SPUStandardUpdaterController(
        startingUpdater: true,
        updaterDelegate: nil,
        userDriverDelegate: nil
    )

    var canCheckForUpdates: Bool { controller.updater.canCheckForUpdates }

    func checkForUpdates() {
        guard canCheckForUpdates else { return }
        // Sparkle owns app replacement and relaunch. Startup reconciliation is
        // the only commit point for bringing the embedded daemon into sync.
        controller.checkForUpdates(nil)
    }
}
