import Foundation
import PlugIPC

/// The apps on this Mac, split by the one thing a person checks first: is it
/// talking to Plug right now.
struct AppRoster: Equatable {
    struct Connected: Identifiable, Equatable {
        var id: String { app.target }
        let app: LinkableApp
        let sessions: [LiveSession]
    }

    /// Apps with an open session, each with the sessions that belong to it.
    let connected: [Connected]
    /// Installed or configured apps with nothing open.
    let idle: [LinkableApp]
    /// Open sessions from something Plug has no app entry for.
    let other: [LiveSession]

    /// `named` is the client a session's owner named it as, when that is a
    /// client Plug knows. It outranks what the session reports: a sign-in
    /// named Grok Bot is not Cursor's, though it says Cursor.
    init(
        apps: [LinkableApp],
        sessions: [LiveSession],
        named: (LiveSession) -> String? = { _ in nil }
    ) {
        var claimed = Set<String>()
        var connected: [Connected] = []
        var idle: [LinkableApp] = []
        for app in apps {
            let own = sessions.filter { session in
                if let target = named(session) { return target == app.target }
                return Self.targets(of: session).contains(app.target)
            }
            claimed.formUnion(own.map(\.sessionId))
            if !own.isEmpty {
                connected.append(Connected(app: app, sessions: own))
            } else {
                idle.append(app)
            }
        }
        self.connected = connected
        self.idle = idle
        self.other = sessions.filter { !claimed.contains($0.sessionId) }
    }

    static func targets(of session: LiveSession) -> Set<String> {
        var targets = [AppIcons.target(forClientType: session.clientType)]
        if let info = session.clientInfo, !info.isEmpty {
            targets.append(AppIcons.target(forClientType: info))
        }
        return Set(targets)
    }
}

extension LiveSession {
    /// The product name of a client Plug knows, nil for any other.
    var knownName: String? {
        for value in [clientType, clientInfo].compactMap({ $0 }) {
            let target = AppIcons.target(forClientType: value)
            if let canonical = AppIcons.displayName(forTarget: target) {
                return canonical
            }
        }
        return nil
    }

    /// What to call a session, best witness first: a client Plug knows, then
    /// the program that started the connector, then whatever the client said
    /// about itself.
    var displayName: String {
        if let knownName { return knownName }
        let type = clientType
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
        let isUnknown = type.localizedCaseInsensitiveCompare("unknown") == .orderedSame
        // An app bundle is a name a person recognises; `python3` is not, so a
        // command line host only wins over a client that said nothing useful.
        if isUnknown, let host, host.app != nil, !host.name.isEmpty {
            return host.name
        }
        if let info = clientInfo,
           !info.isEmpty,
           info.localizedCaseInsensitiveCompare("mcp") != .orderedSame
        {
            return info
        }
        guard isUnknown else { return type }
        if let host, !host.name.isEmpty { return host.name }
        return "Unknown client \(sessionId.prefix(4))"
    }
}

/// The names the owner gave clients, and which client each session is.
///
/// A name belongs to a client, not a session, so it is looked up through the
/// key the daemon stores it under.
struct ClientNames: Equatable {
    private let keys: [String: String]
    private let names: [String: String]
    /// The name each remote client registered its grant under.
    private let grantNames: [String: String]
    /// Where the owner says each client runs.
    private let places: [String: String]

    init(
        visibility: [ClientVisibility],
        names: [ClientName],
        grants: [DownstreamClient] = [],
        places: [ClientPlace] = []
    ) {
        self.places = Dictionary(places.map { ($0.key, $0.place) }, uniquingKeysWith: { first, _ in first })
        keys = Dictionary(
            visibility.compactMap { entry in entry.clientKey.map { (entry.sessionId, $0) } },
            uniquingKeysWith: { first, _ in first }
        )
        self.names = Dictionary(names.map { ($0.key, $0.name) }, uniquingKeysWith: { first, _ in first })
        grantNames = Dictionary(
            grants.filter { !$0.clientName.isEmpty }.map { ($0.clientKey, $0.clientName) },
            uniquingKeysWith: { first, _ in first }
        )
    }

    init(snapshot: OperatorSnapshot) {
        self.init(
            visibility: snapshot.clientVisibility,
            names: snapshot.clientNames ?? [],
            grants: snapshot.downstreamClients,
            places: snapshot.clientPlaces ?? []
        )
    }

    /// Nil for a session the daemon cannot tell apart from other clients.
    /// There is nothing to store a name under, so it cannot be renamed.
    func key(of session: LiveSession) -> String? { keys[session.sessionId] }

    func name(forKey key: String?) -> String? { key.flatMap { names[$0] } }

    /// Where the owner says the client runs.
    func place(forKey key: String?) -> String? { key.flatMap { places[$0] } }

    /// Every place the owner has typed, in order.
    var ownerPlaces: [String] {
        Set(places.values).sorted { $0.localizedStandardCompare($1) == .orderedAscending }
    }

    /// The owner's name first, then a product Plug knows. A remote session
    /// Plug does not know takes the name on its grant, which the owner
    /// approved, ahead of whatever the session says about itself.
    func displayName(_ session: LiveSession) -> String {
        name(forKey: key(of: session)) ?? originalName(session)
    }

    /// What a session is called before the owner names it.
    func originalName(_ session: LiveSession) -> String {
        let key = key(of: session)
        return session.knownName
            // A link says which client it was written for; that outranks
            // the program that happened to start the connector.
            ?? key.flatMap(AppIcons.displayName(forTarget:))
            ?? key.flatMap { grantNames[$0] }
            ?? session.displayName
    }

    /// The client a name belongs to, when the name is one Plug knows. An
    /// owner who names a client "Grok Bot" has said what it is, whatever the
    /// client reports about itself.
    func knownTarget(named name: String?) -> String? {
        guard let name else { return nil }
        let target = AppIcons.target(forClientType: name)
        return AppIcons.displayName(forTarget: target) == nil ? nil : target
    }

    /// The name a remote client is pictured by: the owner's, when it is a
    /// client Plug knows, else the one on its grant.
    func picturedName(forGrant grant: DownstreamClient) -> String {
        let owned = name(forKey: grant.clientKey)
        return knownTarget(named: owned) == nil ? grant.clientName : (owned ?? grant.clientName)
    }

    /// The target whose icon a session shows: the client its owner named it
    /// as, else its own when the owner made it up in Add a Client, else the
    /// client it reports, else the client its link was written for, else
    /// the client its sign-in names. A call is pictured the same way.
    func target(of session: LiveSession) -> String {
        if let named = knownTarget(named: name(forKey: key(of: session))) { return named }
        if let key = key(of: session), key.hasPrefix(AddClient.customPrefix) { return key }
        let reported = AppIcons.target(forClientType: session.clientType)
        if AppIcons.displayName(forTarget: reported) != nil { return reported }
        if let key = key(of: session), AppIcons.displayName(forTarget: key) != nil {
            return key
        }
        if let grant = key(of: session).flatMap({ grantNames[$0] }) {
            return AppIcons.target(forClientType: grant)
        }
        return reported
    }

    /// The distinct clients behind a set of open sessions, first seen first,
    /// each as the Clients list names and pictures it.
    func connectedClients(_ sessions: [LiveSession]) -> [ConnectedClient] {
        var seen = Set<String>()
        return sessions.compactMap { session in
            let target = target(of: session)
            let known = AppIcons.displayName(forTarget: target) != nil
            // A client Plug knows is one client however it is keyed. Any
            // other is told apart by its key.
            let identity = known ? target : (key(of: session) ?? target)
            guard seen.insert(identity).inserted else { return nil }
            return ConnectedClient(
                target: target,
                name: displayName(session),
                appPath: session.host?.app
            )
        }
    }
}

/// Clients known only by the calls they made: not connected now, and with
/// no entry of their own.
enum RecentCallers {
    /// The last call of each client that is not already shown, newest
    /// first. A call with no key belongs to no client Plug can tell apart.
    static func latest(_ events: [ActivityEvent], excluding shown: Set<String>) -> [ActivityEvent] {
        var last: [String: ActivityEvent] = [:]
        for event in events {
            guard let key = event.clientKey, !key.isEmpty, !shown.contains(key) else { continue }
            if last[key].map({ $0.occurredAtMs < event.occurredAtMs }) ?? true { last[key] = event }
        }
        return last.values.sorted { $0.occurredAtMs > $1.occurredAtMs }
    }

    /// Whether a key belongs to a client that reaches Plug over the network.
    static func isRemote(key: String) -> Bool {
        key.hasPrefix("oauth:") || key.hasPrefix("remote:")
    }

    /// "Last used 11:15 AM" today, and with the day before that.
    static func lastUsed(_ date: Date, now: Date = Date(), calendar: Calendar = .current) -> String {
        let time = date.formatted(date: .omitted, time: .shortened)
        if calendar.isDate(date, inSameDayAs: now) { return "Last used \(time)" }
        return "Last used \(date.formatted(.dateTime.month(.abbreviated).day())), \(time)"
    }
}

/// The sections of the Clients column. A client sits under how it reaches
/// Plug unless the owner has said where it runs; then it sits under that.
enum ClientPlaces {
    /// The section a client is listed under.
    static func place(owners: String?, group: ClientEntry.Group) -> String {
        if group != .notUsing, let owners, !owners.isEmpty { return owners }
        return group.rawValue
    }

    /// The order the sections show in: on this Mac, the owner's places by
    /// name, over the network, then clients that do not use Plug.
    static func ordered(_ places: [String]) -> [String] {
        let groups = ClientEntry.Group.self
        let last = [groups.network.rawValue, groups.notUsing.rawValue, groups.unfinished.rawValue]
        let present = Set(places)
        let owners = present.subtracting(last + [groups.onThisMac.rawValue])
            .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
        return ([groups.onThisMac.rawValue] + owners + last).filter(present.contains)
    }
}

/// Telling one sign-in over the network from another by when it happened.
enum SignInFacts {
    /// How long a new sign-in keeps asking which client it is.
    static let newFor: TimeInterval = 14 * 86_400

    static func day(_ seconds: UInt64, now: Date = Date()) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(seconds))
        if Calendar.current.isDate(date, inSameDayAs: now) { return "today" }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }

    /// What an unfinished sign-in's row says in place of a switch.
    static func goneIn(expiresAt: UInt64?, now: Date = Date()) -> String {
        guard let expiresAt else { return "" }
        let left = TimeInterval(expiresAt) - now.timeIntervalSince1970
        if left < 60 { return "gone soon" }
        if left < 3_600 { return "gone in \(Int(left / 60)) min" }
        return "gone in \(Int(left / 3_600)) hr"
    }

    /// The earlier sign-ins a new one may be replacing: the ones that gave
    /// the same name. Empty once the owner has named the new one or answered.
    static func earlier(
        than grant: DownstreamClient,
        among grants: [DownstreamClient],
        ownerNamed: Bool,
        answered: Set<String>,
        now: Date = Date()
    ) -> [DownstreamClient] {
        guard let created = grant.createdAt, !grant.needsSignIn, !ownerNamed,
              !answered.contains(grant.clientId),
              now.timeIntervalSince1970 - TimeInterval(created) < newFor
        else { return [] }
        return grants.filter {
            $0.clientId != grant.clientId && !$0.isUnfinished
                && $0.clientName.caseInsensitiveCompare(grant.clientName) == .orderedSame
                && ($0.createdAt ?? 0) <= created
        }
    }
}
