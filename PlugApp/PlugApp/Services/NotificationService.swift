import Foundation
import UserNotifications
import PlugIPC

@MainActor
final class NotificationService {
    static let shared = NotificationService()

    typealias NotificationSink = @MainActor @Sendable (String, String, String) -> Void

    private let sink: NotificationSink
    private var previous: OperatorSnapshot?

    init(sink: @escaping NotificationSink = NotificationService.enqueue) {
        self.sink = sink
    }

    func requestAuthorization() {
        Task {
            _ = try? await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound])
        }
    }

    func observe(_ snapshot: OperatorSnapshot) {
        defer { previous = snapshot }
        guard let previous else { return }

        let oldAuth = Dictionary(uniqueKeysWithValues: previous.upstreamAuth.map { ($0.name, $0.authenticated) })
        for server in snapshot.upstreamAuth where !server.authenticated && oldAuth[server.name] == true {
            post(
                id: "upstream-reauth-\(server.name)",
                title: "\(server.name) needs sign-in",
                body: "Open Plug to reconnect this server."
            )
        }

        let oldClients = Set(previous.downstreamClients.map(\.clientId))
        for client in snapshot.downstreamClients where !oldClients.contains(client.clientId) {
            post(
                id: "downstream-client-\(client.clientId)",
                title: "New client connected",
                body: "\(client.clientName) can now use Plug."
            )
        }
    }

    private func post(id: String, title: String, body: String) {
        sink(id, title, body)
    }

    private static func enqueue(id: String, title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        let request = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}
