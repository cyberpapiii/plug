import Foundation
import UserNotifications
import PlugIPC

@MainActor
final class NotificationService {
    static let shared = NotificationService()

    /// One notification, as posted. `server` is set when the notification is
    /// about one server, and makes it clickable.
    struct Note: Equatable, Sendable {
        let id: String
        let title: String
        let body: String
        var server: String?
    }

    typealias NotificationSink = @MainActor @Sendable (Note) -> Void

    nonisolated static let signInCategory = "plug.server.sign-in"
    nonisolated static let signInAction = "plug.server.sign-in.action"
    nonisolated static let serverKey = "server"

    private let sink: NotificationSink
    private var previous: OperatorSnapshot?
    private let responder = NotificationResponder()
    private var pendingIntent: PlugIntent?

    /// Where a clicked notification goes. A click that arrives before the
    /// interface has set this, such as the one that launched the app, waits.
    var perform: ((PlugIntent) -> Void)? {
        didSet {
            guard let perform, let pendingIntent else { return }
            self.pendingIntent = nil
            perform(pendingIntent)
        }
    }

    init(sink: @escaping NotificationSink = NotificationService.enqueue) {
        self.sink = sink
    }

    /// Registers the Sign In action and takes clicks. Runs at launch, so a
    /// click that opened the app is not delivered to nobody.
    func install() {
        let center = UNUserNotificationCenter.current()
        center.setNotificationCategories([
            UNNotificationCategory(
                identifier: Self.signInCategory,
                actions: [UNNotificationAction(identifier: Self.signInAction, title: "Sign In")],
                intentIdentifiers: []
            ),
        ])
        center.delegate = responder
    }

    /// Asks macOS for permission. False when the person has said no, in which
    /// case only System Settings can change it.
    func requestAuthorization() async -> Bool {
        (try? await UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound])) ?? false
    }

    func observe(_ snapshot: OperatorSnapshot) {
        defer { previous = snapshot }
        guard let previous else { return }

        let oldAuth = Dictionary(uniqueKeysWithValues: previous.upstreamAuth.map { ($0.name, $0.authenticated) })
        for server in snapshot.upstreamAuth where !server.authenticated && oldAuth[server.name] == true {
            post(Note(
                id: "upstream-reauth-\(server.name)",
                title: "\(server.name) needs sign-in",
                body: "Click to open it in Plug, or sign in from here.",
                server: server.name
            ))
        }

        let oldClients = Set(previous.downstreamClients.map(\.clientId))
        for client in snapshot.downstreamClients where !oldClients.contains(client.clientId) {
            post(Note(
                id: "downstream-client-\(client.clientId)",
                title: "New client connected",
                body: "\(client.clientName) can now use Plug."
            ))
        }
    }

    /// What a click on a server's notification does: the body opens that
    /// server, the Sign In button starts its sign-in, and a dismissal does
    /// nothing.
    nonisolated static func intent(forAction action: String, server: String?) -> PlugIntent? {
        guard let server else { return nil }
        switch action {
        case signInAction: return .signIn(server: server)
        case UNNotificationDefaultActionIdentifier: return .reveal(server: server)
        default: return nil
        }
    }

    func handle(action: String, server: String?) {
        guard let intent = Self.intent(forAction: action, server: server) else { return }
        if let perform { perform(intent) } else { pendingIntent = intent }
    }

    /// Notifications interrupt, so they remain off until the person explicitly
    /// asks for them in Settings.
    private var isEnabled: Bool {
        UserDefaults.standard.object(forKey: NotificationService.preferenceKey) as? Bool ?? false
    }

    static let preferenceKey = "notificationsEnabled"

    private func post(_ note: Note) {
        guard isEnabled else { return }
        sink(note)
    }

    private static func enqueue(_ note: Note) {
        let content = UNMutableNotificationContent()
        content.title = note.title
        content.body = note.body
        content.sound = .default
        if let server = note.server {
            content.categoryIdentifier = signInCategory
            content.userInfo = [serverKey: server]
        }
        let request = UNNotificationRequest(identifier: note.id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}

/// Without a delegate, clicking a notification only brings the app forward,
/// and a menu bar app has nothing to bring forward.
private final class NotificationResponder: NSObject, UNUserNotificationCenterDelegate {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let action = response.actionIdentifier
        let server = response.notification.request.content.userInfo[NotificationService.serverKey] as? String
        await NotificationService.shared.handle(action: action, server: server)
    }
}
