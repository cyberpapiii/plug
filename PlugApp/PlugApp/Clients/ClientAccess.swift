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
    let blockedServers: Set<String>
    /// Tools kept from the client, lowercased, as the daemon compares
    /// them. A name with `*` in it is a rule and covers every tool it fits.
    let blockedTools: [String]
    var blockedToolCount: Int { blockedTools.count }
    /// The client gets only the servers and tools on its list, so a server
    /// added later is off for it until it is turned on.
    let onlyAllowed: Bool
    let allowedServers: Set<String>
    /// Servers of which only single tools are on the list.
    let partlyAllowedServers: Set<String>
    /// Those tools, lowercased.
    let allowedTools: Set<String>

    init(key: String, name: String, servers: [ConfiguredServer], blocks: [ClientBlocks]) {
        self.key = key
        self.name = name
        isRemote = key.hasPrefix("oauth:")
        self.servers = servers
        let blocks = blocks.first { $0.key == key }
        // A block on a server that is gone does nothing and is not counted.
        blockedServers = Set(blocks?.servers ?? []).intersection(servers.map(\.name))
        blockedTools = (blocks?.tools ?? []).map { $0.lowercased() }
        onlyAllowed = blocks?.onlyAllowed ?? false
        allowedServers = Set(blocks?.allowedServers ?? [])
        partlyAllowedServers = Set(blocks?.partlyAllowedServers ?? []).subtracting(allowedServers)
        allowedTools = Set((blocks?.allowedTools ?? []).map { $0.lowercased() })
    }

    /// Whether the client gets anything of `server`.
    func isOn(server: String) -> Bool {
        if blockedServers.contains(server) { return false }
        return !onlyAllowed || allowedServers.contains(server) || partlyAllowedServers.contains(server)
    }

    /// The servers the client gets nothing of.
    var offServers: [String] { servers.map(\.name).filter { !isOn(server: $0) } }

    /// Whether this tool's switch puts it on the list or takes it off: its
    /// server is not on the list whole. Otherwise the switch is a block.
    func isListedSingly(_ tool: ToolFacts) -> Bool {
        onlyAllowed && !allowedServers.contains(tool.server)
    }

    func state(of tool: ToolFacts) -> ToolState {
        if isListedSingly(tool), !allowedTools.contains(tool.name.lowercased()) { return .off }
        return state(ofTool: tool.name)
    }

    /// How a tool stands for this client.
    enum ToolState: Equatable {
        case on
        /// Off by its own name; a switch turns it back on.
        case off
        /// Off under a rule that covers other tools as well.
        case offByRule(String)
    }

    func state(ofTool name: String) -> ToolState {
        let name = name.lowercased()
        if blockedTools.contains(name) { return .off }
        if let rule = blockedTools.first(where: { $0.contains("*") && Self.rule($0, fits: name) }) {
            return .offByRule(rule)
        }
        return .on
    }

    /// How many of these tools the client is kept from.
    func offCount(among tools: [ToolFacts]) -> Int {
        tools.filter { state(of: $0) != .on }.count
    }

    /// Whether `text` fits a rule in which `*` stands for any run of
    /// characters, the way the daemon reads one.
    static func rule(_ rule: String, fits text: String) -> Bool {
        let parts = rule.components(separatedBy: "*")
        guard parts.count > 1 else { return rule == text }
        var rest = Substring(text)
        for (index, part) in parts.enumerated() where !part.isEmpty {
            if index == 0 {
                guard rest.hasPrefix(part) else { return false }
                rest = rest.dropFirst(part.count)
            } else if index == parts.count - 1 {
                return rest.hasSuffix(part)
            } else {
                guard let found = rest.range(of: part) else { return false }
                rest = rest[found.upperBound...]
            }
        }
        return true
    }

    var isLimited: Bool { !offServers.isEmpty || blockedToolCount > 0 }

    /// What the status line says when something is off for the client.
    var summary: String? {
        guard isLimited else { return nil }
        var parts: [String] = []
        if !offServers.isEmpty {
            let count = offServers.count
            parts.append(count == 1 ? "1 server" : "\(count) servers")
        }
        if blockedToolCount > 0 {
            parts.append(blockedToolCount == 1 ? "1 tool" : "\(blockedToolCount) tools")
        }
        return parts.joined(separator: " and ") + " off"
    }
}
