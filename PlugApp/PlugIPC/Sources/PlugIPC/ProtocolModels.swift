import Foundation

/// Who started the daemon, as launchd records it.
public enum DaemonOwnership: String, Codable, Sendable {
    case unmanaged
    /// The daemon could not read its own launchd registration.
    case unknown
    case cliManaged = "cli_managed"
    case appManaged = "app_managed"
}

/// An upstream server's health, as the daemon names it.
public enum DaemonHealth: String, Codable, Sendable {
    case healthy = "Healthy"
    case degraded = "Degraded"
    case failed = "Failed"
    case authRequired = "AuthRequired"
}

/// How a watched event is doing.
public enum WatchState: String, Codable, Sendable {
    case waiting
    case watching
    case toolMissing = "tool_missing"
    case notReadOnly = "not_read_only"
    case callFailed = "call_failed"
    case tooLarge = "too_large"
}

/// How a call ended.
public enum CallOutcome: String, Codable, Sendable {
    case success
    case error
    case cancelled
}

public struct OperatorHandshake: Codable, Equatable, Sendable {
    public let daemonVersion: String
    public let daemonExecutable: URL?
    public let ownership: DaemonOwnership

    private enum CodingKeys: String, CodingKey {
        case daemonVersion, daemonExecutable, ownership
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        daemonVersion = try container.decode(String.self, forKey: .daemonVersion)
        if let value = try container.decodeIfPresent(String.self, forKey: .daemonExecutable) {
            daemonExecutable = value.contains("://")
                ? URL(string: value)
                : URL(fileURLWithPath: value)
        } else {
            daemonExecutable = nil
        }
        ownership = try container.decode(DaemonOwnership.self, forKey: .ownership)
    }
}

public struct ServerStatus: Codable, Identifiable, Equatable, Sendable {
    public var id: String { serverId }
    public let serverId: String
    public let health: DaemonHealth
    public let toolCount: Int
    public let error: String?
    /// What the server said about itself when it connected.
    public var upstream: UpstreamInfo?
}

/// The part of a server's own description the app shows: where its maker
/// lives on the web, and the icons it offers for itself.
public struct UpstreamInfo: Codable, Equatable, Sendable {
    public var websiteUrl: String?
    /// Absent in the polled snapshot, which leaves icons out to stay small.
    public var icons: [ServerIcon]?
}

/// One icon a server offers for itself: a `data:` or `https:` address.
public struct ServerIcon: Codable, Equatable, Sendable {
    public let src: String
    public var mimeType: String?
    public var sizes: [String]?

    public init(src: String, mimeType: String? = nil, sizes: [String]? = nil) {
        self.src = src
        self.mimeType = mimeType
        self.sizes = sizes
    }
}

public struct ConfiguredServer: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    public let name: String
    public let enabled: Bool
    public let transport: String
    public let oauth: Bool
}

public struct LiveSession: Codable, Identifiable, Equatable, Sendable {
    public var id: String { sessionId }
    public let transport: String
    public let clientId: String?
    public let sessionId: String
    public let clientType: String
    public let clientInfo: String?
    /// The program that started a local connector. The daemon reads it from
    /// the process table, so the client cannot choose it.
    public let host: ClientHost?
    /// What the client's settings are stored under, when the daemon can tell
    /// one such client from another.
    public let clientKey: String?
    /// The key the session's requests carry, which blocks are read under. It
    /// differs from `clientKey` only for a remote session with no grant,
    /// which shares one key with every other such session.
    public let accessKey: String?
    /// The client Plug takes this one for, by its link target: the one its
    /// link names, else the one its reported name is.
    public let target: String?
}

public struct ClientHost: Codable, Equatable, Sendable {
    /// The app bundle's name, or the executable's file name.
    public let name: String
    public let executable: String
    /// The app bundle the executable runs from, when it runs from one.
    public let app: String?
}

/// A name the owner gave a client.
public struct ClientName: Codable, Equatable, Sendable {
    public let key: String
    public let name: String
}

/// Where the owner says a client runs.
public struct ClientPlace: Codable, Equatable, Sendable {
    public let key: String
    public let place: String
}

/// What the owner keeps a client from. A list the daemon left out is empty.
public struct ClientBlocks: Codable, Equatable, Sendable {
    public let key: String
    public var servers: [String]?
    public var tools: [String]?
    /// The client gets only what the two lists below name.
    public var onlyAllowed: Bool?
    public var allowedServers: [String]?
    public var allowedTools: [String]?
    /// The configured servers the client gets nothing of, as the daemon
    /// enforces the lists above.
    public var offServers: [String]?
    /// The listed tools of every other server that the client is kept from.
    public var offTools: [OffTool]?
}

/// A listed tool a client is kept from.
public struct OffTool: Codable, Equatable, Sendable {
    public let name: String
    /// The `*` rule among the client's blocks that covers this tool, when one does.
    public let rule: String?
}

public struct AuthServer: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    public let name: String
    public let url: String?
    public let authenticated: Bool
    public let health: DaemonHealth
    public let scopes: [String]?
    public var tokenExpiresInSecs: UInt64?
    public let warnings: [String]
}

public struct DownstreamClient: Codable, Identifiable, Equatable, Sendable {
    public var id: String { clientId }
    public let clientId: String
    public let clientName: String
    public let redirectUris: [String]
    public let source: String
    /// False when the client holds no token that still works.
    public var signedIn: Bool
    /// When the client asked to be let in, in seconds since 1970.
    public var createdAt: UInt64
    /// When it last signed in or renewed. Nil for one that never has.
    public var lastUsedAt: UInt64?
    /// When Plug forgets it unless it signs in again before then.
    public var expiresAt: UInt64
    /// What this client's settings are stored under.
    public let clientKey: String
    /// The client Plug takes this one for by the name it registered.
    public let target: String?

    /// Its sign-in ran out or was revoked, and only the client can start
    /// another.
    public var needsSignIn: Bool { !signedIn }

    /// It asked to be let in and nobody finished the sign-in. Plug forgets
    /// it at `expiresAt`.
    public var isUnfinished: Bool { !signedIn && lastUsedAt == nil }
}

/// One event a client can subscribe to, and how it is doing.
public struct EventStatus: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    /// `<server>.<name>`.
    public let name: String
    public let server: String
    /// The watched tool. Nil for an event Plug does not make by watching.
    public let tool: String?
    public let everySecs: UInt64?
    public let state: WatchState
    /// Unix seconds of the last check that reached the tool.
    public let lastChecked: UInt64?
    /// Unix seconds of the last change that became an event.
    public let lastChanged: UInt64?
    public let subscribers: Int

    public init(
        name: String,
        server: String,
        tool: String? = nil,
        everySecs: UInt64? = nil,
        state: WatchState = .waiting,
        lastChecked: UInt64? = nil,
        lastChanged: UInt64? = nil,
        subscribers: Int = 0
    ) {
        self.name = name
        self.server = server
        self.tool = tool
        self.everySecs = everySecs
        self.state = state
        self.lastChecked = lastChecked
        self.lastChanged = lastChanged
        self.subscribers = subscribers
    }

}

/// Any JSON value: the arguments a watched tool is called with, and the
/// server settings the app has no field for.
public enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    /// Nil for anything JSON cannot hold.
    public init?(_ value: Any) {
        switch value {
        case is NSNull: self = .null
        case let number as NSNumber:
            // JSONSerialization hands back booleans as NSNumber too.
            self = CFGetTypeID(number) == CFBooleanGetTypeID() ? .bool(number.boolValue) : .number(number.doubleValue)
        case let string as String: self = .string(string)
        case let array as [Any]:
            var values: [JSONValue] = []
            for item in array {
                guard let value = JSONValue(item) else { return nil }
                values.append(value)
            }
            self = .array(values)
        case let object as [String: Any]:
            var values: [String: JSONValue] = [:]
            for (key, item) in object {
                guard let value = JSONValue(item) else { return nil }
                values[key] = value
            }
            self = .object(values)
        default: return nil
        }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let values = try? container.decode([JSONValue].self) {
            self = .array(values)
        } else {
            // A dictionary keeps its keys as written, as on the way out.
            self = .object(try container.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case let .bool(value): try container.encode(value)
        case let .number(value):
            // A whole number goes out as one, so a tool that wants an
            // integer is not handed `5.0`.
            if let whole = Int64(exactly: value) {
                try container.encode(whole)
            } else {
                try container.encode(value)
            }
        case let .string(value): try container.encode(value)
        case let .array(values): try container.encode(values)
        // A dictionary keeps its keys as written. A keyed container would
        // have them rewritten to snake case on the way out.
        case let .object(values): try container.encode(values)
        }
    }
}

/// A tool Plug calls on a timer, sending an event when the result changes.
public struct WatchConfig: Encodable, Equatable, Sendable {
    /// The event is named `<server>.<name>`.
    public var name: String
    public var server: String
    /// The tool's own name on that server, without Plug's prefix.
    public var tool: String
    public var arguments: [String: JSONValue]
    public var everySecs: UInt64
    public var allowWrites: Bool

    public init(
        name: String,
        server: String,
        tool: String,
        arguments: [String: JSONValue] = [:],
        everySecs: UInt64 = 300,
        allowWrites: Bool = false
    ) {
        self.name = name
        self.server = server
        self.tool = tool
        self.arguments = arguments
        self.everySecs = everySecs
        self.allowWrites = allowWrites
    }
}

public struct OperatorSnapshot: Codable, Equatable, Sendable {
    public let runtimeVersion: String
    public var uptimeSecs: UInt64
    /// Changes when the daemon's tool list would answer differently. The
    /// snapshot is cheap and polled often; the tool list is nearly a megabyte
    /// and almost never changes between two polls.
    public var toolCatalogRevision: UInt64
    public let ownership: DaemonOwnership
    public let configuredServers: [ConfiguredServer]
    public let servers: [ServerStatus]
    public var liveSessions: [LiveSession]
    public var upstreamAuth: [AuthServer]
    public let downstreamClients: [DownstreamClient]
    /// Names the owner gave clients. Absent when there are none.
    public var clientNames: [ClientName]?
    /// Where the owner says each client runs. Absent when none is said.
    public var clientPlaces: [ClientPlace]?
    /// What each client is kept from. Absent when nobody is kept from anything.
    public var clientBlocks: [ClientBlocks]?
    /// The events clients can subscribe to. Absent when there are none.
    public var events: [EventStatus]?
    /// The address a client Plug has no switch for is given to reach it.
    public var clientAddress: String?
    /// What joins a server's prefix to a tool's name in the names clients
    /// call: `Server__tool` by default, as the config sets it.
    public var prefixDelimiter: String

    /// Before the first read nothing is known, so no name is split.
    public static let empty = OperatorSnapshot(
        runtimeVersion: "", uptimeSecs: 0, toolCatalogRevision: 0, ownership: .unmanaged,
        configuredServers: [], servers: [], liveSessions: [],
        upstreamAuth: [], downstreamClients: [], prefixDelimiter: ""
    )
}

public extension OperatorSnapshot {
    /// The same snapshot with its clocks rounded down, so two reads a moment
    /// apart are equal unless something happened. Every count of seconds in
    /// it ticks on its own, and a reader that redraws on any difference
    /// would redraw on every read.
    ///
    /// A reader asks one thing of the daemon's first `startup` seconds:
    /// whether they are over. So the uptime reads 0 until then and more
    /// than `startup` after, rounded to the minute.
    func steadied(startup: UInt64) -> OperatorSnapshot {
        var steady = self
        steady.uptimeSecs = uptimeSecs > startup ? max(startup + 1, uptimeSecs / 60 * 60) : 0
        for index in steady.upstreamAuth.indices {
            steady.upstreamAuth[index].tokenExpiresInSecs = upstreamAuth[index].tokenExpiresInSecs.map {
                $0 >= 3_600 ? $0 / 3_600 * 3_600 : $0 / 60 * 60
            }
        }
        return steady
    }
}

public struct ActivityEvent: Codable, Identifiable, Equatable, Sendable {
    /// The key every remote caller without a sign-in of its own shares.
    public static let sharedRemoteKey = "remote:shared"

    public var id: UInt64 { sequence }
    public let sequence: UInt64
    public let occurredAtMs: UInt64
    /// Per-connection identity. For a local editor this is a per-process UUID,
    /// so it separates one window from another but means nothing to a reader.
    public let client: String?
    public let method: String
    public let server: String?
    public let tool: String?
    public let clientType: String?
    public let clientLabel: String?
    /// The key the client's name is kept under. Nil for a caller Plug
    /// cannot tell apart.
    public let clientKey: String?
    /// The client Plug took the caller for, by its link target.
    public let target: String?
    /// The call came over the network rather than from this Mac.
    public let remote: Bool
    public let latencyMs: UInt64
    public let outcome: CallOutcome
    /// Why a failed call failed, in the error's own words. Nil for a call
    /// that worked.
    public let reason: String?

    public init(
        sequence: UInt64,
        occurredAtMs: UInt64,
        client: String?,
        method: String,
        server: String?,
        tool: String? = nil,
        clientType: String? = nil,
        clientLabel: String? = nil,
        clientKey: String? = nil,
        target: String? = nil,
        remote: Bool = false,
        latencyMs: UInt64,
        outcome: CallOutcome,
        reason: String? = nil
    ) {
        self.sequence = sequence
        self.occurredAtMs = occurredAtMs
        self.client = client
        self.method = method
        self.server = server
        self.tool = tool
        self.clientType = clientType
        self.clientLabel = clientLabel
        self.clientKey = clientKey
        self.target = target
        self.remote = remote
        self.latencyMs = latencyMs
        self.outcome = outcome
        self.reason = reason
    }
}

public struct ToolInfo: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    /// Merged name downstream clients call, already server-prefixed.
    public let name: String
    public let serverId: String
    public let description: String?
    public let title: String?
    /// Hidden from downstream clients by a `disabled_tools` entry.
    public let disabled: Bool
    /// Set when a wildcard, rather than this tool's own name, is what hides it.
    public let disabledByPattern: String?
    /// The name the server itself gives the tool, before any prefix or rename.
    public let ownName: String?
    /// The server says calling this tool changes nothing.
    public let readOnly: Bool

    public init(
        name: String,
        serverId: String,
        description: String? = nil,
        title: String? = nil,
        disabled: Bool = false,
        disabledByPattern: String? = nil,
        ownName: String? = nil,
        readOnly: Bool = false
    ) {
        self.name = name
        self.serverId = serverId
        self.description = description
        self.title = title
        self.disabled = disabled
        self.disabledByPattern = disabledByPattern
        self.ownName = ownName
        self.readOnly = readOnly
    }

    private enum CodingKeys: String, CodingKey {
        case name, serverId, description, title, disabled, disabledByPattern, ownName, readOnly
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        serverId = try container.decode(String.self, forKey: .serverId)
        description = try container.decodeIfPresent(String.self, forKey: .description)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        disabled = try container.decodeIfPresent(Bool.self, forKey: .disabled) ?? false
        disabledByPattern = try container.decodeIfPresent(String.self, forKey: .disabledByPattern)
        ownName = try container.decodeIfPresent(String.self, forKey: .ownName)
        readOnly = try container.decodeIfPresent(Bool.self, forKey: .readOnly) ?? false
    }
}

/// A server as the config holds it. A setting the app has no field for is
/// nil until the daemon sends it, and a nil one is left out of what the app
/// sends, so the daemon's own default applies (`ServerConfig::default` in
/// `plug-core/src/config`).
public struct ServerConfig: Codable, Equatable, Sendable {
    public var command: String?
    public var args: [String] = []
    public var env: [String: String] = [:]
    public var enabled = true
    public var transport: String
    public var protocolMode: String?
    public var url: String?
    public var authToken: String?
    public var auth: String?
    public var oauthClientID: String?
    public var oauthScopes: [String]?
    public var timeoutSecs: Int?
    public var callTimeoutSecs: Int?
    public var maxConcurrent: Int?
    public var healthCheckIntervalSecs: Int?
    public var circuitBreakerEnabled: Bool?
    public var toolRenames: [String: String] = [:]
    public var toolGroups: [ToolGroupRule] = []
    public var sandbox: StdioSandboxConfig?
    /// The OpenAPI document of an API server, and the operations it exposes.
    /// The app has no fields for these; it carries them so a save keeps them.
    public var spec: String?
    public var operations: [String] = []
    /// Where an API server sends its token; the app carries it unchanged.
    public var tokenIn: String?
    /// Settings the daemon sent that this type has no field for, sent back
    /// as they came. The daemon stores the server it is sent whole, so
    /// without them a save from an app older than the daemon would erase
    /// them.
    public var unmodeled: [String: JSONValue] = [:]

    public static func command(_ command: String, args: [String]) -> Self {
        Self(command: command, args: args, transport: "stdio")
    }

    public static func remote(_ url: String) -> Self {
        Self(transport: "http", url: url)
    }

    /// An HTTP API, named by the URL or file path of its OpenAPI document.
    /// Four calls at once, as `plug servers add` gives an API server.
    public static func api(_ spec: String) -> Self {
        Self(transport: "openapi", maxConcurrent: 4, spec: spec)
    }
}

extension ServerConfig {
    private enum CodingKeys: String, CodingKey {
        case command, args, env, enabled, transport, protocolMode = "protocol", url, authToken
        // As the snake-case decoder spells `oauth_client_id`; `oauthClientID`
        // never matched it, so a read lost the field and a save dropped it.
        case auth, oauthClientID = "oauthClientId", oauthScopes, timeoutSecs, callTimeoutSecs, maxConcurrent
        case healthCheckIntervalSecs, circuitBreakerEnabled, toolRenames, toolGroups
        case sandbox, spec, operations, tokenIn
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        command = try c.decodeIfPresent(String.self, forKey: .command)
        args = try c.decodeIfPresent([String].self, forKey: .args) ?? []
        env = try c.decodeIfPresent([String: String].self, forKey: .env) ?? [:]
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        transport = try c.decodeIfPresent(String.self, forKey: .transport) ?? "stdio"
        protocolMode = try c.decodeIfPresent(String.self, forKey: .protocolMode)
        url = try c.decodeIfPresent(String.self, forKey: .url)
        authToken = try c.decodeIfPresent(String.self, forKey: .authToken)
        auth = try c.decodeIfPresent(String.self, forKey: .auth)
        oauthClientID = try c.decodeIfPresent(String.self, forKey: .oauthClientID)
        oauthScopes = try c.decodeIfPresent([String].self, forKey: .oauthScopes)
        timeoutSecs = try c.decodeIfPresent(Int.self, forKey: .timeoutSecs)
        callTimeoutSecs = try c.decodeIfPresent(Int.self, forKey: .callTimeoutSecs)
        maxConcurrent = try c.decodeIfPresent(Int.self, forKey: .maxConcurrent)
        healthCheckIntervalSecs = try c.decodeIfPresent(Int.self, forKey: .healthCheckIntervalSecs)
        circuitBreakerEnabled = try c.decodeIfPresent(Bool.self, forKey: .circuitBreakerEnabled)
        toolRenames = try c.decodeIfPresent([String: String].self, forKey: .toolRenames) ?? [:]
        toolGroups = try c.decodeIfPresent([ToolGroupRule].self, forKey: .toolGroups) ?? []
        sandbox = try c.decodeIfPresent(StdioSandboxConfig.self, forKey: .sandbox)
        spec = try c.decodeIfPresent(String.self, forKey: .spec)
        operations = try c.decodeIfPresent([String].self, forKey: .operations) ?? []
        tokenIn = try c.decodeIfPresent(String.self, forKey: .tokenIn)
        let all = try decoder.container(keyedBy: AnyKey.self)
        for key in all.allKeys where CodingKeys(stringValue: key.stringValue) == nil {
            unmodeled[key.stringValue] = try all.decode(JSONValue.self, forKey: key)
        }
    }

    /// Leaves out what is nil and an empty `operations`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(command, forKey: .command)
        try c.encode(args, forKey: .args)
        try c.encode(env, forKey: .env)
        try c.encode(enabled, forKey: .enabled)
        try c.encode(transport, forKey: .transport)
        try c.encodeIfPresent(protocolMode, forKey: .protocolMode)
        try c.encodeIfPresent(url, forKey: .url)
        try c.encodeIfPresent(authToken, forKey: .authToken)
        try c.encodeIfPresent(auth, forKey: .auth)
        try c.encodeIfPresent(oauthClientID, forKey: .oauthClientID)
        try c.encodeIfPresent(oauthScopes, forKey: .oauthScopes)
        try c.encodeIfPresent(timeoutSecs, forKey: .timeoutSecs)
        try c.encodeIfPresent(callTimeoutSecs, forKey: .callTimeoutSecs)
        try c.encodeIfPresent(maxConcurrent, forKey: .maxConcurrent)
        try c.encodeIfPresent(healthCheckIntervalSecs, forKey: .healthCheckIntervalSecs)
        try c.encodeIfPresent(circuitBreakerEnabled, forKey: .circuitBreakerEnabled)
        try c.encode(toolRenames, forKey: .toolRenames)
        try c.encode(toolGroups, forKey: .toolGroups)
        try c.encodeIfPresent(sandbox, forKey: .sandbox)
        try c.encodeIfPresent(spec, forKey: .spec)
        if !operations.isEmpty { try c.encode(operations, forKey: .operations) }
        try c.encodeIfPresent(tokenIn, forKey: .tokenIn)
        // Each key is the one the decoder handed over, which the encoder's
        // key strategy turns back into the daemon's spelling.
        var rest = encoder.container(keyedBy: AnyKey.self)
        for (key, value) in unmodeled {
            try rest.encode(value, forKey: AnyKey(stringValue: key))
        }
    }
}

/// A key known only when it is read.
private struct AnyKey: CodingKey {
    let stringValue: String
    var intValue: Int? { nil }

    init(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

public struct ToolGroupRule: Codable, Equatable, Sendable {
    public var prefix: String
    public var contains: [String]
    public var strip: [String] = []
}

public struct StdioSandboxConfig: Codable, Equatable, Sendable {
    public var enabled = false
    public var allowNetwork = false
    public var allowRead: [String] = []
    public var allowWrite: [String] = []
    public var profilePath: String?
}

/// What an API offers, read from its OpenAPI document.
public struct APISummary: Decodable, Equatable, Sendable {
    /// The most operations one API server may expose.
    public static let operationLimit = 50

    public var title: String
    public var operations: [APIOperation]

    public init(title: String, operations: [APIOperation]) {
        self.title = title
        self.operations = operations
    }
}

/// One operation of an API, named as the tool it becomes.
public struct APIOperation: Decodable, Equatable, Sendable, Identifiable {
    public var name: String
    public var method: String
    public var path: String
    public var summary: String
    public var tag: String?
    public var id: String { name }

    public init(name: String, method: String, path: String, summary: String, tag: String? = nil) {
        self.name = name
        self.method = method
        self.path = path
        self.summary = summary
        self.tag = tag
    }
}

/// What the app sends the daemon. Every request but these few needs the
/// daemon's token and travels as `.operator`.
public enum IPCRequest: Encodable, Equatable, Sendable {
    case handshake(clientVersion: String)
    /// Every server with what it said about itself, icons included.
    case status
    case listTools
    case `operator`(token: String, OperatorRequest)

    private enum CodingKeys: String, CodingKey {
        case type, clientVersion, authToken, request
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .handshake(version):
            try c.encode("OperatorHandshake", forKey: .type); try c.encode(version, forKey: .clientVersion)
        case .status:
            try c.encode("Status", forKey: .type)
        case .listTools:
            try c.encode("ListTools", forKey: .type)
        case let .operator(token, request):
            try c.encode("Operator", forKey: .type); try c.encode(token, forKey: .authToken)
            try c.encode(request, forKey: .request)
        }
    }
}

/// A request only the holder of the daemon's token may make.
public enum OperatorRequest: Encodable, Equatable, Sendable {
    case snapshot
    case activity(afterSequence: UInt64, limit: Int, failuresOnly: Bool)
    case serverConfig(name: String)
    case validateServer(name: String, server: ServerConfig)
    /// List the operations of an OpenAPI document before its server exists.
    case describeAPI(spec: String)
    /// `secretStore` names where the keys typed into a server are kept: the
    /// Keychain when nil, or `file` for Plug's `.env` file.
    case mutate(OperatorMutation, secretStore: String? = nil)
    case restartServer(serverID: String)
    case reload
    case revokeClient(clientID: String)
    case shutdown

    private enum CodingKeys: String, CodingKey {
        case type, afterSequence, limit, failuresOnly, name, server, spec, mutation, secretStore
        case serverID, clientID
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .snapshot:
            try c.encode("Snapshot", forKey: .type)
        case let .activity(after, limit, failures):
            try c.encode("Activity", forKey: .type)
            try c.encode(after, forKey: .afterSequence); try c.encode(limit, forKey: .limit)
            try c.encode(failures, forKey: .failuresOnly)
        case let .serverConfig(name):
            try c.encode("GetServerConfig", forKey: .type); try c.encode(name, forKey: .name)
        case let .validateServer(name, server):
            try c.encode("ValidateServer", forKey: .type)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
        case let .describeAPI(spec):
            try c.encode("DescribeApi", forKey: .type); try c.encode(spec, forKey: .spec)
        case let .mutate(mutation, store):
            try c.encode("Mutate", forKey: .type); try c.encode(mutation, forKey: .mutation)
            try c.encodeIfPresent(store, forKey: .secretStore)
        case let .restartServer(serverID):
            try c.encode("RestartServer", forKey: .type); try c.encode(serverID, forKey: .serverID)
        case .reload:
            try c.encode("Reload", forKey: .type)
        case let .revokeClient(clientID):
            try c.encode("RevokeDownstreamClient", forKey: .type); try c.encode(clientID, forKey: .clientID)
        case .shutdown:
            try c.encode("Shutdown", forKey: .type)
        }
    }
}

/// One change to the config file, which the daemon saves and applies.
public enum OperatorMutation: Encodable, Equatable, Sendable {
    case addServer(name: String, server: ServerConfig)
    /// Add servers found in other clients' settings, all or none. A name
    /// already taken goes in with its client's slug after it, as
    /// `plug import` names it.
    case importServers([ImportedServer])
    case updateServer(name: String, server: ServerConfig)
    case removeServer(name: String)
    /// Add a configured server again as `<server>-<account>`.
    case addAccount(server: String, account: String)
    case setServerEnabled(name: String, enabled: Bool)
    case setToolEnabled(tool: String, enabled: Bool)
    /// An empty name goes back to the name Plug works out.
    case renameClient(key: String, name: String)
    /// Hand one client's settings to another, replacing what that one had.
    case moveClientSettings(from: String, to: String)
    /// An empty place goes back to the place Plug works out.
    case setClientPlace(key: String, place: String)
    /// For the allow-list kinds `blocked` means on the list.
    case setClientBlock(key: String, kind: ClientBlockKind, target: String, blocked: Bool)
    /// Start watching a tool. The daemon checks the tool before it saves.
    case addWatch(WatchConfig)
    case removeWatch(event: String)

    private enum CodingKeys: String, CodingKey {
        case type, name, server, enabled, tool, key, kind, target, blocked, place, watch, event
        case account, from, to, servers
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .addServer(name, server):
            try c.encode("AddServer", forKey: .type)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
        case let .importServers(servers):
            try c.encode("ImportServers", forKey: .type); try c.encode(servers, forKey: .servers)
        case let .updateServer(name, server):
            try c.encode("UpdateServer", forKey: .type)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
        case let .removeServer(name):
            try c.encode("RemoveServer", forKey: .type); try c.encode(name, forKey: .name)
        case let .addAccount(server, account):
            try c.encode("AddAccount", forKey: .type)
            try c.encode(server, forKey: .server); try c.encode(account, forKey: .account)
        case let .setServerEnabled(name, enabled):
            try c.encode("SetServerEnabled", forKey: .type)
            try c.encode(name, forKey: .name); try c.encode(enabled, forKey: .enabled)
        case let .setToolEnabled(tool, enabled):
            try c.encode("SetToolEnabled", forKey: .type)
            try c.encode(tool, forKey: .tool); try c.encode(enabled, forKey: .enabled)
        case let .renameClient(key, name):
            try c.encode("RenameClient", forKey: .type)
            try c.encode(key, forKey: .key); try c.encode(name, forKey: .name)
        case let .moveClientSettings(from, to):
            try c.encode("MoveClientSettings", forKey: .type)
            try c.encode(from, forKey: .from); try c.encode(to, forKey: .to)
        case let .setClientPlace(key, place):
            try c.encode("SetClientPlace", forKey: .type)
            try c.encode(key, forKey: .key); try c.encode(place, forKey: .place)
        case let .setClientBlock(key, kind, target, blocked):
            try c.encode("SetClientBlock", forKey: .type)
            try c.encode(key, forKey: .key); try c.encode(kind, forKey: .kind)
            try c.encode(target, forKey: .target); try c.encode(blocked, forKey: .blocked)
        case let .addWatch(watch):
            try c.encode("AddWatch", forKey: .type); try c.encode(watch, forKey: .watch)
        case let .removeWatch(event):
            try c.encode("RemoveWatch", forKey: .type); try c.encode(event, forKey: .event)
        }
    }
}

/// A server found in another client's settings, on its way into the config.
public struct ImportedServer: Encodable, Equatable, Sendable {
    public var name: String
    /// The slug of the client it was found in.
    public var sourceTarget: String
    public var server: ServerConfig

    public init(name: String, sourceTarget: String, server: ServerConfig) {
        self.name = name
        self.sourceTarget = sourceTarget
        self.server = server
    }
}

/// What a per-client block names.
public enum ClientBlockKind: String, Encodable, Sendable {
    case server
    case tool
    /// A server on the client's allow list.
    case allowedServer = "allowed_server"
    case allowedTool = "allowed_tool"
    /// The allow list itself; its target is `*`.
    case allowList = "allow_list"
}

public enum IPCResponse: Decodable, Sendable {
    case handshake(OperatorHandshake)
    case snapshot(OperatorSnapshot)
    case status([ServerStatus])
    case serverConfig(name: String, server: ServerConfig)
    case activity([ActivityEvent])
    case tools([ToolInfo])
    case validated
    case apiDescribed(APISummary)
    /// The names the servers of an import went in under; empty for any other change.
    case mutation(imported: [String])
    case revoked(String)
    case reloaded
    case ok
    case error(code: String, message: String)

    private enum CodingKeys: String, CodingKey {
        case type, handshake, snapshot, events, tools, clientId, code, message, name, server
        case api, servers, result
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "OperatorHandshake": self = .handshake(try c.decode(OperatorHandshake.self, forKey: .handshake))
        case "OperatorSnapshot": self = .snapshot(try c.decode(OperatorSnapshot.self, forKey: .snapshot))
        case "Status": self = .status(try c.decode([ServerStatus].self, forKey: .servers))
        case "ServerConfig": self = .serverConfig(
            name: try c.decode(String.self, forKey: .name),
            server: try c.decode(ServerConfig.self, forKey: .server)
        )
        case "ActivitySnapshot": self = .activity(try c.decode([ActivityEvent].self, forKey: .events))
        case "Tools": self = .tools(try c.decode([ToolInfo].self, forKey: .tools))
        case "ServerValidated": self = .validated
        case "ApiDescribed": self = .apiDescribed(try c.decode(APISummary.self, forKey: .api))
        case "OperatorMutation":
            struct Result: Decodable {
                struct Imported: Decodable { let name: String }
                let imported: [Imported]?
            }
            let result = try c.decodeIfPresent(Result.self, forKey: .result)
            self = .mutation(imported: result?.imported?.map(\.name) ?? [])
        case "DownstreamClientRevoked": self = .revoked(try c.decode(String.self, forKey: .clientId))
        case "Reloaded": self = .reloaded
        case "Ok": self = .ok
        case "Error": self = .error(code: try c.decode(String.self, forKey: .code), message: try c.decode(String.self, forKey: .message))
        default: throw PlugIPCError.unexpectedResponse(type)
        }
    }
}
