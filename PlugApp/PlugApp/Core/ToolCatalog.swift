import Foundation
import PlugIPC

/// One tool a connected app can call, in the terms the interface needs.
///
/// Tools arrive from the daemon under their merged name (`figma__get_file`),
/// which is what a client actually calls. People read them per server, so the
/// server prefix is stripped for display and kept for every action.
struct ToolFacts: Identifiable, Equatable, Sendable {
    var id: String { name }
    let name: String
    let server: String
    let summary: String?
    let isOn: Bool
    /// The wildcard hiding this tool, when it is not switched off by name.
    /// A tool inside a wildcard cannot be switched back on by itself.
    let lockedByPattern: String?

    init(
        name: String,
        server: String,
        summary: String? = nil,
        isOn: Bool = true,
        lockedByPattern: String? = nil
    ) {
        self.name = name
        self.server = server
        self.summary = summary
        self.isOn = isOn
        self.lockedByPattern = lockedByPattern
    }

    /// The name without its server prefix, which the surrounding group already
    /// states. Falls back to the full name when there is no prefix to drop.
    var shortName: String {
        let prefix = "\(server)__"
        guard name.count > prefix.count,
              name.lowercased().hasPrefix(prefix.lowercased())
        else { return name }
        return String(name.dropFirst(prefix.count))
    }

    var canToggle: Bool { lockedByPattern == nil }
}

extension ToolFacts {
    init(_ info: ToolInfo) {
        self.init(
            name: info.name,
            server: info.serverId,
            summary: info.description ?? info.title,
            isOn: !info.disabled,
            lockedByPattern: info.disabledByPattern
        )
    }
}

/// Tools of one server, kept together because that is how people look for them.
struct ToolGroup: Identifiable, Equatable, Sendable {
    var id: String { server }
    let server: String
    let tools: [ToolFacts]

    var onCount: Int { tools.filter(\.isOn).count }
    var isFullyOff: Bool { !tools.isEmpty && onCount == 0 }
}

/// The full tool list, grouped and searchable. Pure value, so the rules for
/// what a search matches and how groups are ordered can be tested directly.
struct ToolCatalog: Equatable, Sendable {
    let tools: [ToolFacts]

    init(_ tools: [ToolFacts] = []) {
        self.tools = tools
    }

    var isEmpty: Bool { tools.isEmpty }
    var onCount: Int { tools.filter(\.isOn).count }
    var offCount: Int { tools.count - onCount }

    /// Groups matching a query, servers alphabetical, tools alphabetical.
    ///
    /// A query matches a tool's name or its description, and also matches every
    /// tool of a server whose name matches — searching "figma" should show what
    /// Figma can do, not nothing.
    func groups(matching query: String = "") -> [ToolGroup] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let matched = needle.isEmpty ? tools : tools.filter { $0.matches(needle) }
        return Dictionary(grouping: matched, by: \.server)
            .map { server, tools in
                ToolGroup(server: server, tools: tools.sorted { $0.shortName < $1.shortName })
            }
            .sorted { $0.server.localizedStandardCompare($1.server) == .orderedAscending }
    }

    /// Every tool a wildcard holds off. A pattern is the only way a tool can be
    /// off without being named, so the panel that explains one has to be able
    /// to list what else it took.
    func tools(coveredBy pattern: String) -> [ToolFacts] {
        tools.filter { $0.lockedByPattern == pattern }.sorted { $0.shortName < $1.shortName }
    }

    func tools(for server: String) -> [ToolFacts] {
        tools.filter { $0.server == server }.sorted { $0.shortName < $1.shortName }
    }
}

private extension ToolFacts {
    func matches(_ needle: String) -> Bool {
        name.lowercased().contains(needle)
            || server.lowercased().contains(needle)
            || (summary?.lowercased().contains(needle) ?? false)
    }
}
