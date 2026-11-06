import Foundation

public struct OperatorHandshake: Codable, Equatable, Sendable {
    public let daemonVersion: String
    public let daemonExecutable: URL?
    public let ownership: String

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
        ownership = try container.decode(String.self, forKey: .ownership)
    }
}

public struct ServerStatus: Codable, Identifiable, Equatable, Sendable {
    public var id: String { serverId }
    public let serverId: String
    public let health: String
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
    public var connectedSecs: UInt64
    public var lastActivitySecs: UInt64?
}

public struct ClientHost: Codable, Equatable, Sendable {
    /// The app bundle's name, or the executable's file name.
    public let name: String
    public let executable: String
    /// The app bundle the executable runs from, when it runs from one.
    public let app: String?
}

public struct ClientVisibility: Codable, Equatable, Sendable {
    public let sessionId: String
    public let clientType: String
    public let visibleToolCount: Int
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
    /// The servers those single tools belong to.
    public var partlyAllowedServers: [String]?
}

public struct AuthServer: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }
    public let name: String
    public let url: String?
    public let authenticated: Bool
    public let health: String
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
    /// False when the client holds no token that still works. Absent from a
    /// daemon older than the field, which is read as signed in.
    public var signedIn: Bool?
    /// When the client asked to be let in, in seconds since 1970.
    public var createdAt: UInt64?
    /// When it last signed in or renewed. Nil for one that never has.
    public var lastUsedAt: UInt64?
    /// When Plug forgets it unless it signs in again before then.
    public var expiresAt: UInt64?

    /// Its sign-in ran out or was revoked, and only the client can start
    /// another.
    public var needsSignIn: Bool { signedIn == false }

    /// It asked to be let in and nobody finished the sign-in. Plug forgets
    /// it at `expiresAt`.
    public var isUnfinished: Bool { signedIn == false && lastUsedAt == nil && createdAt != nil }

    /// What this client's settings are stored under. Matches the daemon's
    /// `grant_client_key`; the snapshot does not carry it.
    public var clientKey: String { Self.clientKey(id: clientId) }

    public static func clientKey(id: String) -> String { "oauth:\(id)" }
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
    /// `waiting`, `watching`, `tool_missing`, `not_read_only`, `call_failed`,
    /// or `too_large`.
    public let state: String
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
        state: String = "waiting",
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

    private enum CodingKeys: String, CodingKey {
        case name, server, tool, everySecs, state, lastChecked, lastChanged, subscribers
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        server = try container.decode(String.self, forKey: .server)
        tool = try container.decodeIfPresent(String.self, forKey: .tool)
        everySecs = try container.decodeIfPresent(UInt64.self, forKey: .everySecs)
        state = try container.decodeIfPresent(String.self, forKey: .state) ?? "waiting"
        lastChecked = try container.decodeIfPresent(UInt64.self, forKey: .lastChecked)
        lastChanged = try container.decodeIfPresent(UInt64.self, forKey: .lastChanged)
        subscribers = try container.decodeIfPresent(Int.self, forKey: .subscribers) ?? 0
    }
}

/// Any JSON value, for the arguments a watched tool is called with.
public enum JSONValue: Encodable, Equatable, Sendable {
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
    public var toolCatalogRevision: UInt64?
    public let ownership: String
    public let configuredServers: [ConfiguredServer]
    public let servers: [ServerStatus]
    public var liveSessions: [LiveSession]
    public let clientVisibility: [ClientVisibility]
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

    public static let empty = OperatorSnapshot(
        runtimeVersion: "", uptimeSecs: 0, ownership: "unmanaged",
        configuredServers: [], servers: [], liveSessions: [], clientVisibility: [],
        upstreamAuth: [], downstreamClients: []
    )
}

public extension OperatorSnapshot {
    /// The same snapshot with its clocks rounded down, so two reads a moment
    /// apart are equal unless something happened. Every count of seconds in
    /// it ticks on its own, and a reader that redraws on any difference
    /// would redraw on every read.
    func steadied() -> OperatorSnapshot {
        var steady = self
        // A reader asks one thing of the first minute: whether it is over.
        steady.uptimeSecs = uptimeSecs > 60 ? max(61, uptimeSecs / 60 * 60) : 0
        for index in steady.liveSessions.indices {
            steady.liveSessions[index].connectedSecs = liveSessions[index].connectedSecs / 60 * 60
            steady.liveSessions[index].lastActivitySecs = liveSessions[index].lastActivitySecs.map { $0 / 60 * 60 }
        }
        for index in steady.upstreamAuth.indices {
            steady.upstreamAuth[index].tokenExpiresInSecs = upstreamAuth[index].tokenExpiresInSecs.map {
                $0 >= 3_600 ? $0 / 3_600 * 3_600 : $0 / 60 * 60
            }
        }
        return steady
    }
}

public struct ActivityEvent: Codable, Identifiable, Equatable, Sendable {
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
    /// The key the client's name is kept under. Absent from a daemon older
    /// than this field.
    public let clientKey: String?
    public let latencyMs: UInt64
    public let outcome: String
    /// Why a failed call failed, in the error's own words. Absent for a call
    /// that worked, and from a daemon older than this field.
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
        latencyMs: UInt64,
        outcome: String,
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

public struct ServerConfig: Codable, Equatable, Sendable {
    public var command: String?
    public var args: [String] = []
    public var env: [String: String] = [:]
    public var enabled = true
    public var transport: String
    public var protocolMode = "legacy"
    public var url: String?
    public var authToken: String?
    public var auth: String?
    public var oauthClientID: String?
    public var oauthScopes: [String]?
    public var timeoutSecs = 30
    public var callTimeoutSecs = 300
    public var maxConcurrent = 1
    public var healthCheckIntervalSecs = 60
    public var circuitBreakerEnabled = true
    public var enrichment = false
    public var toolRenames: [String: String] = [:]
    public var toolGroups: [ToolGroupRule] = []
    public var sandbox: StdioSandboxConfig?
    /// The OpenAPI document of an API server, and the operations it exposes.
    /// The app has no fields for these; it carries them so a save keeps them.
    public var spec: String?
    public var operations: [String] = []
    /// Where an API server sends its token; the app carries it unchanged.
    public var tokenIn: String?

    public static func command(_ command: String, args: [String]) -> Self {
        Self(command: command, args: args, transport: "stdio")
    }

    public static func remote(_ url: String) -> Self {
        Self(transport: "http", url: url)
    }

    /// An HTTP API, named by the URL or file path of its OpenAPI document.
    public static func api(_ spec: String) -> Self {
        Self(transport: "openapi", maxConcurrent: 4, spec: spec)
    }
}

extension ServerConfig {
    private enum CodingKeys: String, CodingKey {
        case command, args, env, enabled, transport, protocolMode = "protocol", url, authToken
        case auth, oauthClientID, oauthScopes, timeoutSecs, callTimeoutSecs, maxConcurrent
        case healthCheckIntervalSecs, circuitBreakerEnabled, enrichment, toolRenames, toolGroups
        case sandbox, spec, operations, tokenIn
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        command = try c.decodeIfPresent(String.self, forKey: .command)
        args = try c.decodeIfPresent([String].self, forKey: .args) ?? []
        env = try c.decodeIfPresent([String: String].self, forKey: .env) ?? [:]
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        transport = try c.decodeIfPresent(String.self, forKey: .transport) ?? "stdio"
        protocolMode = try c.decodeIfPresent(String.self, forKey: .protocolMode) ?? "legacy"
        url = try c.decodeIfPresent(String.self, forKey: .url)
        authToken = try c.decodeIfPresent(String.self, forKey: .authToken)
        auth = try c.decodeIfPresent(String.self, forKey: .auth)
        oauthClientID = try c.decodeIfPresent(String.self, forKey: .oauthClientID)
        oauthScopes = try c.decodeIfPresent([String].self, forKey: .oauthScopes)
        timeoutSecs = try c.decodeIfPresent(Int.self, forKey: .timeoutSecs) ?? 30
        callTimeoutSecs = try c.decodeIfPresent(Int.self, forKey: .callTimeoutSecs) ?? 300
        maxConcurrent = try c.decodeIfPresent(Int.self, forKey: .maxConcurrent) ?? 1
        healthCheckIntervalSecs = try c.decodeIfPresent(Int.self, forKey: .healthCheckIntervalSecs) ?? 60
        circuitBreakerEnabled = try c.decodeIfPresent(Bool.self, forKey: .circuitBreakerEnabled) ?? true
        enrichment = try c.decodeIfPresent(Bool.self, forKey: .enrichment) ?? false
        toolRenames = try c.decodeIfPresent([String: String].self, forKey: .toolRenames) ?? [:]
        toolGroups = try c.decodeIfPresent([ToolGroupRule].self, forKey: .toolGroups) ?? []
        sandbox = try c.decodeIfPresent(StdioSandboxConfig.self, forKey: .sandbox)
        spec = try c.decodeIfPresent(String.self, forKey: .spec)
        operations = try c.decodeIfPresent([String].self, forKey: .operations) ?? []
        tokenIn = try c.decodeIfPresent(String.self, forKey: .tokenIn)
    }

    /// As the daemon writes a server, which leaves out an empty `operations`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(command, forKey: .command)
        try c.encode(args, forKey: .args)
        try c.encode(env, forKey: .env)
        try c.encode(enabled, forKey: .enabled)
        try c.encode(transport, forKey: .transport)
        try c.encode(protocolMode, forKey: .protocolMode)
        try c.encodeIfPresent(url, forKey: .url)
        try c.encodeIfPresent(authToken, forKey: .authToken)
        try c.encodeIfPresent(auth, forKey: .auth)
        try c.encodeIfPresent(oauthClientID, forKey: .oauthClientID)
        try c.encodeIfPresent(oauthScopes, forKey: .oauthScopes)
        try c.encode(timeoutSecs, forKey: .timeoutSecs)
        try c.encode(callTimeoutSecs, forKey: .callTimeoutSecs)
        try c.encode(maxConcurrent, forKey: .maxConcurrent)
        try c.encode(healthCheckIntervalSecs, forKey: .healthCheckIntervalSecs)
        try c.encode(circuitBreakerEnabled, forKey: .circuitBreakerEnabled)
        try c.encode(enrichment, forKey: .enrichment)
        try c.encode(toolRenames, forKey: .toolRenames)
        try c.encode(toolGroups, forKey: .toolGroups)
        try c.encodeIfPresent(sandbox, forKey: .sandbox)
        try c.encodeIfPresent(spec, forKey: .spec)
        if !operations.isEmpty { try c.encode(operations, forKey: .operations) }
        try c.encodeIfPresent(tokenIn, forKey: .tokenIn)
    }
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
        case account, from, to
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .addServer(name, server):
            try c.encode("AddServer", forKey: .type)
            try c.encode(name, forKey: .name); try c.encode(server, forKey: .server)
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

/// What reloading the configuration from disk changed.
public struct ReloadSummary: Decodable, Equatable, Sendable {
    public let added: [String]
    public let removed: [String]
    public let changed: [String]
    public let errors: [String]

    public init(added: [String] = [], removed: [String] = [], changed: [String] = [], errors: [String] = []) {
        self.added = added
        self.removed = removed
        self.changed = changed
        self.errors = errors
    }

    private enum CodingKeys: String, CodingKey {
        case added, removed, changed, errors
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        added = try container.decodeIfPresent([String].self, forKey: .added) ?? []
        removed = try container.decodeIfPresent([String].self, forKey: .removed) ?? []
        changed = try container.decodeIfPresent([String].self, forKey: .changed) ?? []
        errors = try container.decodeIfPresent([String].self, forKey: .errors) ?? []
    }

    /// One line a person can read, naming what actually moved.
    public var summary: String {
        var parts: [String] = []
        if !added.isEmpty { parts.append("\(added.count) added") }
        if !removed.isEmpty { parts.append("\(removed.count) removed") }
        if !changed.isEmpty { parts.append("\(changed.count) changed") }
        if parts.isEmpty { return "Nothing changed" }
        return parts.joined(separator: ", ")
    }
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
    case mutation
    case revoked(String)
    /// What a reload changed. The app does not show it yet.
    case reloaded(ReloadSummary)
    case ok
    case error(code: String, message: String)

    private enum CodingKeys: String, CodingKey {
        case type, handshake, snapshot, events, tools, clientId, code, message, report, name, server
        case api, servers
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
        case "OperatorMutation": self = .mutation
        case "DownstreamClientRevoked": self = .revoked(try c.decode(String.self, forKey: .clientId))
        case "Reloaded": self = .reloaded(try c.decode(ReloadSummary.self, forKey: .report))
        case "Ok": self = .ok
        case "Error": self = .error(code: try c.decode(String.self, forKey: .code), message: try c.decode(String.self, forKey: .message))
        default: throw PlugIPCError.unexpectedResponse(type)
        }
    }
}
