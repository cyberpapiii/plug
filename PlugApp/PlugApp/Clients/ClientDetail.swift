import PlugIPC
import SwiftUI

/// Everything about one client. Its icon, its name, its switch and its two
/// settings are the first thing on the page; the servers it can use follow.
struct ClientDetail: View {
    let entry: ClientEntry
    let tools: ToolCatalog
    /// The places the Runs On menu offers.
    let places: [String]
    let canMutate: Bool
    let run: (PlugIntent) -> Void
    /// The name as it is being typed.
    @State private var draft: String
    @FocusState private var naming: Bool
    /// A place being typed, while the sheet that asks for it is up.
    @State private var newPlace: String?
    /// The servers whose tools are showing.
    @State private var opened: Set<String> = []

    init(
        entry: ClientEntry,
        tools: ToolCatalog,
        places: [String],
        canMutate: Bool,
        run: @escaping (PlugIntent) -> Void
    ) {
        self.entry = entry
        self.tools = tools
        self.places = places
        self.canMutate = canMutate
        self.run = run
        _draft = State(initialValue: entry.name)
    }

    var body: some View {
        DetailForm {
            if let asking = entry.asking {
                Section {
                    question(asking)
                }
            }

            Section {
                header
                if let command = entry.signInCommand {
                    ProblemNote(
                        title: "\(entry.name) needs to sign in again.",
                        actionTitle: "Sign In",
                        action: { run(.signInClient(name: entry.name, command: command)) }
                    )
                    .disabled(!canMutate)
                }
                if let key = entry.placeKey, entry.group != .notUsing {
                    runsOn(key: key)
                }
                if let access = entry.access, !access.servers.isEmpty {
                    Toggle(
                        "New Servers",
                        isOn: Binding(
                            get: { !access.onlyAllowed },
                            set: { run(.setClientAllowList(key: access.key, on: !$0)) }
                        )
                    )
                    .disabled(!canMutate)
                    .help("When this is off, a server you add later stays off for this client until you turn it on below.")
                }
                if let forget = entry.forget {
                    Button("Forget Now", role: .destructive, action: forget)
                        .disabled(!canMutate)
                }
            } footer: {
                Text(entry.about)
            }

            if let access = entry.access {
                Section {
                    if access.servers.isEmpty {
                        Text("No servers yet").foregroundStyle(.secondary)
                    } else {
                        ForEach(access.servers) { server in
                            serverRow(server, access: access)
                        }
                    }
                } header: {
                    SectionLabel(text: "Servers")
                } footer: {
                    Text(Self.note(for: access))
                }
            }
        }
    }

    /// The icon is a menu and the name is a field, so both change where they
    /// are shown.
    private var header: some View {
        HStack(spacing: Metric.snug) {
            Menu {
                IconMenu(key: entry.glyph.iconKey)
            } label: {
                ClientGlyph(glyph: entry.glyph, large: true)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .frame(width: Metric.glyphSlot, height: Metric.glyphSlot)
            .help("Change the icon")
            .accessibilityLabel("Icon of \(entry.name)")
            VStack(alignment: .leading, spacing: Metric.hairline) {
                if let key = entry.renameKey {
                    HStack(spacing: Metric.tight) {
                        TextField("Name", text: $draft, prompt: Text(entry.originalName))
                            .textFieldStyle(.plain)
                            .labelsHidden()
                            .font(.title3.weight(.semibold))
                            .focused($naming)
                            .onSubmit { rename(key) }
                            .onChange(of: naming) { if !naming { rename(key) } }
                            .disabled(!canMutate)
                            .help("Click to rename. Only Plug shows this name.")
                        PlugIcon(.edit, size: 14)
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                    }
                } else {
                    Text(entry.name)
                        .font(.title3.weight(.semibold))
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text(entry.status.text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .layoutPriority(1)
            Spacer(minLength: Metric.tight)
            ClientSwitch(entry: entry, canMutate: canMutate)
        }
    }

    /// An empty name, or the client's own, goes back to the client's own.
    private func rename(_ key: String) {
        let typed = draft.trimmingCharacters(in: .whitespaces)
        let name = typed == entry.originalName ? "" : typed
        if typed.isEmpty { draft = entry.originalName }
        guard (name.isEmpty ? entry.originalName : name) != entry.name else { return }
        run(.renameClient(key: key, name: name))
    }

    /// One question, asked once: a new sign-in gave a name that earlier ones
    /// gave, and only the owner knows whether it is one of them.
    private func question(_ asking: ClientEntry.Asking) -> some View {
        VStack(alignment: .leading, spacing: Metric.tight) {
            Text("New sign-in \(asking.when). Which client is this?")
                .font(.callout.weight(.medium))
            Text("It calls itself \(entry.originalName), like others here. Pick one and this sign-in takes over its name and choices, and the old sign-in is removed.")
                .font(PanelType.small)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                ForEach(asking.choices) { choice in
                    Button(choice.name) { asking.replace(choice.id) }
                }
                Button("A New Client", action: asking.keep)
            }
            .disabled(!canMutate)
        }
    }

    /// Where the client runs: the places already in use, or a new one.
    private func runsOn(key: String) -> some View {
        LabeledContent("Runs On") {
            Menu(entry.hasOwnPlace ? entry.place : "Not Set") {
                ForEach(places, id: \.self) { place in
                    Button(place) { run(.setClientPlace(key: key, place: place)) }
                }
                if !places.isEmpty { Divider() }
                Button("New Place…") { newPlace = "" }
                if entry.hasOwnPlace {
                    Button("Not Set") { run(.setClientPlace(key: key, place: "")) }
                }
            }
            .fixedSize()
            .disabled(!canMutate)
        }
        .alert("Where does \(entry.name) run?", isPresented: Binding(
            get: { newPlace != nil },
            set: { if !$0 { newPlace = nil } }
        )) {
            TextField("Work laptop", text: Binding(get: { newPlace ?? "" }, set: { newPlace = $0 }))
            Button("Save") {
                let place = (newPlace ?? "").trimmingCharacters(in: .whitespaces)
                if !place.isEmpty { run(.setClientPlace(key: key, place: place)) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("A name for the computer this client runs on. Clients with a place are listed under it.")
        }
    }

    /// One server's switch for this client, and under it, when opened, a
    /// switch for each of its tools.
    @ViewBuilder private func serverRow(_ server: ConfiguredServer, access: ClientAccess) -> some View {
        let serverOn = access.isOn(server: server.name)
        let own = tools.tools(for: server.name).filter(\.isOn)
        let off = access.offCount(among: own)
        let isOpen = opened.contains(server.name) && server.enabled && serverOn && !own.isEmpty
        HStack(spacing: Metric.tight) {
            Button {
                if isOpen { opened.remove(server.name) } else { opened.insert(server.name) }
            } label: {
                Image(systemName: "chevron.right")
                    .font(PanelType.small.weight(.semibold))
                    .rotationEffect(.degrees(isOpen ? 90 : 0))
                    .frame(width: 12)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .opacity(server.enabled && serverOn && !own.isEmpty ? 1 : 0)
            .accessibilityLabel(isOpen ? "Hide \(server.name) Tools" : "Show \(server.name) Tools")
            .help(isOpen ? "Hide Tools" : "Show Tools")
            ServerGlyph(name: server.name).opacity(server.enabled ? 1 : 0.4)
            Text(server.name)
            Spacer(minLength: Metric.tight)
            if !server.enabled {
                // Off in Plug, so no client can use it.
                Text("Off")
            } else if serverOn, off > 0 {
                Text(off == 1 ? "1 tool off" : "\(off) tools off")
                    .font(.callout)
            }
            Toggle(
                server.name,
                isOn: Binding(
                    get: { serverOn },
                    set: {
                        run(
                            access.onlyAllowed
                                ? .setClientServerAllowed(key: access.key, server: server.name, allowed: $0)
                                : .setClientServerBlocked(key: access.key, server: server.name, blocked: !$0)
                        )
                    }
                )
            )
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.mini)
            .disabled(!canMutate || !server.enabled)
        }
        .foregroundStyle(server.enabled ? .primary : .secondary)
        if isOpen {
            ForEach(own) { tool in
                toolRow(tool, access: access)
            }
        }
    }

    private func toolRow(_ tool: ToolFacts, access: ClientAccess) -> some View {
        let state = access.state(of: tool)
        return HStack(spacing: Metric.tight) {
            ToolName(tool.shortName, dimmed: state != .on)
                .help(tool.summary ?? tool.shortName)
            Spacer(minLength: Metric.tight)
            if case let .offByRule(rule) = state {
                Label("Off by Rule", icon: .locked)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .help("A rule in the settings file (\(rule)) keeps this tool from this client. Remove the rule to turn it back on.")
            } else {
                Toggle(
                    tool.shortName,
                    isOn: Binding(
                        get: { state == .on },
                        set: {
                            run(
                                access.isListedSingly(tool)
                                    ? .setClientToolAllowed(key: access.key, tool: tool.name, allowed: $0)
                                    : .setClientToolBlocked(key: access.key, tool: tool.name, blocked: !$0)
                            )
                        }
                    )
                )
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
                .disabled(!canMutate)
            }
        }
        .padding(.leading, 44)
    }

    static func note(for access: ClientAccess) -> String {
        access.isRemote
            ? "Turn a server off to keep this client from using its tools. Open a server to turn off single tools."
            : "Turn a server off to hide its tools from this client, or open it to hide single tools. This tidies the list; it is not a security lock."
    }
}
