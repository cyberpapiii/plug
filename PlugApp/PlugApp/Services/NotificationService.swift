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
        /// Set when it is about one client allowed in over the network; a
        /// click opens that client.
        var client: String?
    }

    typealias NotificationSink = @MainActor @Sendable (Note) -> Void

    nonisolated static let signInCategory = "plug.server.sign-in"
    nonisolated static let signInAction = "plug.server.sign-in.action"
    nonisolated static let serverKey = "server"
    nonisolated static let clientKey = "client"

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

        // A sign-in that ends is silent everywhere else: the client only
        // finds out the next time it tries.
        let signedOut = Set(previous.downstreamClients.filter(\.needsSignIn).map(\.clientId))
        let known = Set(previous.downstreamClients.map(\.clientId))
        for client in snapshot.downstreamClients
        where client.needsSignIn && known.contains(client.clientId) && !signedOut.contains(client.clientId) {
            post(Note(
                id: "downstream-sign-in-\(client.clientId)",
                title: "\(client.clientName) needs sign-in",
                body: "Its sign-in to Plug ended. Open Plug's Clients to see how to sign it in again."
            ))
        }

        // Asking to be let in is not news; being let in is. A client that
        // was already signed in when it was first seen counts too.
        let wasIn = Set(previous.downstreamClients.filter { !$0.needsSignIn }.map(\.clientId))
        let hadSignedIn = Set(previous.downstreamClients.filter { !$0.isUnfinished }.map(\.clientId))
        for client in snapshot.downstreamClients
        where !client.needsSignIn && !wasIn.contains(client.clientId) && !hadSignedIn.contains(client.clientId) {
            post(Note(
                id: "downstream-client-\(client.clientId)",
                title: "\(client.clientName) signed in to Plug",
                body: "It can now use your servers. Click to name it or choose what it can use.",
                client: client.clientId
            ))
        }
    }

    /// What a click on a server's notification does: the body opens that
    /// server, the Sign In button starts its sign-in, and a dismissal does
    /// nothing.
    nonisolated static func intent(forAction action: String, server: String?, client: String? = nil) -> PlugIntent? {
        if let client {
            return action == UNNotificationDefaultActionIdentifier ? .revealClient(id: client) : nil
        }
        guard let server else { return nil }
        switch action {
        case signInAction: return .signIn(server: server)
        case UNNotificationDefaultActionIdentifier: return .reveal(server: server)
        default: return nil
        }
    }

    func handle(action: String, server: String?, client: String? = nil) {
        guard let intent = Self.intent(forAction: action, server: server, client: client) else { return }
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
        } else if let client = note.client {
            content.userInfo = [clientKey: client]
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
        let info = response.notification.request.content.userInfo
        await NotificationService.shared.handle(
            action: action,
            server: info[NotificationService.serverKey] as? String,
            client: info[NotificationService.clientKey] as? String
        )
    }
}
