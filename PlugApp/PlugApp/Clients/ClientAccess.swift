import PlugIPC

/// What one client may reach, as its row offers it. A block belongs to a
/// client, so it is looked up through the key its requests carry.
struct ClientAccess: Equatable {
    let key: String
    let name: String
    /// A remote client is held to its blocks. One on this Mac is not: it can
    /// connect under another name.
    let isRemote: Bool
    let servers: [ConfiguredServer]
    /// The servers the client gets nothing of, as the daemon enforces its lists.
    let offServers: [String]
    /// The tools of every other server kept from it, lowercased, each with
    /// the rule that covers it when a rule does.
    private let offTools: [String: String?]
    /// The client gets only the servers and tools on its list, so a server
    /// added later is off for it until it is turned on.
    let onlyAllowed: Bool
    let allowedServers: Set<String>

    init(
        key: String,
        name: String,
        isRemote: Bool = false,
        servers: [ConfiguredServer],
        blocks: [ClientBlocks]
    ) {
        self.key = key
        self.name = name
        self.isRemote = isRemote
        self.servers = servers
        let blocks = blocks.first { $0.key == key }
        offServers = blocks?.offServers ?? []
        offTools = Dictionary(
            (blocks?.offTools ?? []).map { ($0.name.lowercased(), $0.rule) },
            uniquingKeysWith: { first, _ in first }
        )
        onlyAllowed = blocks?.onlyAllowed ?? false
        allowedServers = Set(blocks?.allowedServers ?? [])
    }

    /// Whether the client gets anything of `server`.
    func isOn(server: String) -> Bool { !offServers.contains(server) }

    /// Whether this tool's switch puts it on the list or takes it off: its
    /// server is not on the list whole. Otherwise the switch is a block.
    func isListedSingly(_ tool: ToolFacts) -> Bool {
        onlyAllowed && !allowedServers.contains(tool.server)
    }

    /// How a tool stands for this client.
    enum ToolState: Equatable {
        case on
        /// Off by its own name; a switch turns it back on.
        case off
        /// Off under a rule that covers other tools as well.
        case offByRule(String)
    }

    func state(of tool: ToolFacts) -> ToolState { state(ofTool: tool.name) }

    func state(ofTool name: String) -> ToolState {
        guard let rule = offTools[name.lowercased()] else { return .on }
        return rule.map(ToolState.offByRule) ?? .off
    }

    /// How many of these tools the client is kept from.
    func offCount(among tools: [ToolFacts]) -> Int {
        tools.filter { state(of: $0) != .on }.count
    }

    var isLimited: Bool { !offServers.isEmpty || !offTools.isEmpty }

    /// What the status line says when something is off for the client.
    var summary: String? {
        guard isLimited else { return nil }
        var parts: [String] = []
        if !offServers.isEmpty {
            let count = offServers.count
            parts.append(count == 1 ? "1 server" : "\(count) servers")
        }
        if !offTools.isEmpty {
            parts.append(offTools.count == 1 ? "1 tool" : "\(offTools.count) tools")
        }
        return parts.joined(separator: " and ") + " off"
    }
}
