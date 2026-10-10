# Changelog

All notable changes to plug are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The sign-in pages Plug shows in the browser, where you approve a client with your passkey and where signing in from the command line lands, carry Plug's icon and look, in light and dark. In the app, a page that is loading shows Plug looking from side to side instead of a spinner, Plug hops when a first-run step is done or when you click it, and the menu bar icon looks from side to side while Plug is busy.
- Plug has a new icon, a plug with a face, in the app, the menu bar, the README, and the icon clients show for Plug. In the menu bar it is awake when Plug is on and asleep when Plug is off, and it blinks now and then. Beside everything Plug says about itself, in the menu bar panel and the window, it blinks when all is well, looks from side to side while Plug starts, shakes its head when something is wrong, hops when that is fixed, and dozes when Plug is off. It holds still when Reduce Motion is on. The rules for the icon, its colours and its motion are in `docs/BRAND.md`.
- The README is rewritten around what Plug is today, with diagrams, and the settings file has its own page, `docs/SETTINGS.md`. Stale lines in the other docs are corrected. The README now says who Plug is for and why it exists, with a picture of every client wired to every server without Plug, what you can do with it, what Plug takes care of, and answers to common questions.
- The setup guide the app hands to an agent now sends keys to the Keychain with `plug secret set`, where it said to use an environment variable.

### Fixed

- A call in flight while Plug renews a server's sign-in is answered. Renewing swapped the connection out from under the call, which came back as "disconnected before answering".
- Servers are checked within ten seconds of the network coming back or changing, such as a new Wi-Fi network or a VPN. Each one used to wait out its own timer, up to a minute.
- A tool call that runs longer than about a minute and a half through the tunnel is answered. The tunnel used to cut it off after a hundred seconds of silence, so Plug now keeps the reply open with a heartbeat.
- A call made at the moment Plug restarts is sent to the new copy when it had not gone out yet. It used to come back as an error asking for a retry.
- A full session list makes room by dropping the session that has been quiet longest, where a new client was refused.
- Client sign-ins recover by themselves after a disk write problem clears. They used to stay refused until Plug restarted.
- The live stream from a network server that went away is retried at most a minute apart. The wait used to double without limit, so a server gone for ten minutes was next tried seventeen minutes later.
- Opening Plug while it is still starting its servers waits for it, where it was stopped and started again from scratch.
- A setup check that fails is tried again five minutes later by itself, and a registration that fails once is tried a second time. Both used to wait for someone to press Try Again.
- `plug start` no longer stops a copy of Plug that started a moment earlier.
- Logs keep being written after a disk cleaner empties the log folder. Nothing was recorded until the next day.
- A call is no longer sent twice when a server drops the connection before answering. Plug used to reconnect and send it again, so a message could go out twice. The caller is now told the server disconnected and the call may or may not have gone through. Calls a server marks as read-only or safe to repeat are still retried.
- A local server that quits is noticed at once and started again. It could sit dead for up to a minute, until the next call failed.
- Servers are checked right after the Mac wakes from sleep, where each one waited out its own timer, up to a minute.
- A server that fails to start after a config change keeps being retried, and one that needs a sign-in says so. It used to stay down until Plug restarted.
- A server busy with a long call is no longer judged unresponsive and restarted in the middle of it.
- Reading a resource or a prompt from a server that hangs gives up after the call timeout, where it waited forever.
- A client in regular use keeps its sign-in. A registration ended ninety days after it was made no matter how often the client was used.
- A client that reconnects after Plug restarted is told to read its tool list again, over the network and on this Mac, and gets the notices waiting for it. It could keep a stale list and miss every change after that.
- Plug can hold 4096 open files, up from 256. With many servers and clients it could run out, and then nothing new could connect.
- A Slack events secret that cannot be read leaves Slack events off and everything else working. It used to stop Plug from serving network clients at all.
- Clients paused for an update are woken when Plug.app next opens, in case the app quit before it could wake them.
- The log no longer warns every minute about a server that has no resources or prompts.
- A client that was connected when Plug restarted, or that sat quiet for half an hour, is taken back the next time it calls. Plug used to answer "session not found" to every call from then on, and some clients, several Grok Bot bots among them, never recovered until they were set up again.
- A client's page starts with everything you change about it: click its icon to pick another, click its name to rename it, and its switch, where it runs and whether it gets new servers sit right below. The Details, Settings and Connected Now sections are gone.
- Plug sends a notification when a client signs in, and clicking it opens that client.
- A new sign-in under a name other clients already use is marked New and asks which client it is. Pick one and it takes over that client's name, place and server choices, and the old sign-in is removed.
- A sign-in that was started and never finished is listed under Unfinished Sign-Ins, says when Plug will forget it, and can be forgotten right away.
- The checkup no longer warns about clients signed in more than once when you have given each one its own name, and it points to the Clients page instead of a terminal command.
- A client that leaves the `MCP-Protocol-Version` label off its requests, as Grok Bot does, is served. Plug refused every one of its calls.
- Plug's log says when it refuses a request from a client and why. A refused client left no trace.
- The menu bar icon opens its panel after Plug was closed by a quit-all and
  came back by itself. It came back hidden, and a hidden app shows nothing.
- Codex CLI keeps its sign-in when one of its processes sat idle for more
  than a week before renewing with a copy another had already used. The
  allowance is now thirty days, as long as Plug remembers a used copy.
- A client allowed single tools of a server gets those tools only, not the
  server's resources and prompts.
- Server icons no longer turn into a letter in a colored square after a disk
  cleaner runs. Found icons are kept with Plug's own files, not in the
  system cache a cleaner empties, and a search cut short while Plug starts
  is no longer remembered as "this site has no icon" for a day.
- A client with an allow list keeps Plug's own search tool, so it can still
  find the tools it is allowed when Plug serves tools by search.
- Removing a server takes it off every client's allow list, so a server
  added later under the same name is not handed to those clients unasked.
- A sign-in ended because its renewal token was used twice stays ended even
  when Plug could not save the change.
- An allowed tool Plug no longer lists is shown as `server/tool`, the form
  `plug clients only --remove --tool` takes.
- A watch on a tool Plug does not list right now is hidden from a client
  whose allow list does not name that tool.
- The large result of a task called by its server's own tool name is no
  longer kept from the client that ran it.

## [0.11.0] - 2026-10-07

### Added

- A client can be given only the servers you pick. Switch off "Adopt Newly
  Added Servers" under Settings on a client's page and a server you add later stays off for that client
  until you turn it on; what the client gets today does not change. The
  same from the command line with `plug clients only <client> --server
  <name>` or `--tool <name>`, and `--off` to end it.
- Clients lists a client that connects, makes its calls, and leaves again,
  such as a script on a timer. It shows where it runs, with "Last used" and
  the time, and can be named, given an icon, and kept from servers and tools
  like any other client. It stays listed while its last call is in Activity.
- Plug's menu bar icon is there whenever Plug is serving. It comes back on
  its own when something else closes the app (a quit-all, a crash, a force
  quit) and appears after login. Settings has a "Keep in Menu Bar" switch
  for it, on by default.
- Settings has a Permissions section: whether macOS lets Plug run in the
  background, open at login, and send notifications, with the way to System
  Settings beside anything that was turned off there.
- A server whose tools Plug splits into groups says so on its page: a
  Clients See row lists the groups, such as Gmail, GoogleDrive, GoogleSheets.
- A Google Workspace server gets its Gmail, GoogleDrive and other groups
  under any name you give it, not only `workspace`: Plug goes by what the
  server says it is.
- Two servers that would give a tool the same name no longer leave only one
  of them. Each gets its own name added to the prefix, so two Google servers
  show as `GmailWorkspace__…` and `GmailGoogleWork__…`.
- A client kept from a tool is no longer sent that tool's results through
  an event watch. Listing, subscribing and delivery all check the tool, and
  an event already queued is dropped when the tool is blocked.
- A tool switched off, or kept from a client, stays that way when a second
  account changes its name (`Gmail__…` to `GmailWorkspace__…`) and when that
  account is removed again.
- Two servers whose names differ only in punctuation (`google-work`,
  `google_work`) each get their own tool names; one is numbered. If two
  servers still claim one name, the second is left out and the log says so,
  instead of one silently replacing the other.
- Codex CLI can be signed in again from its page in Plug: a Sign In button
  runs its sign-in and opens the approval in your browser. Other clients
  still sign in from inside themselves, and their page says how.
- A client can be given a place: the computer it runs on, such as "Work
  laptop". Its page has a Runs On menu for it, and clients with a place are
  listed under it. The rest stay under On This Mac and Over the Network.
  Activity and `plug clients` show the place beside the client's name, and
  `plug clients place <client> <place>` sets it from the command line.
- Each server's page has a Settings row that says where the server's own
  settings are and takes you there: the app it runs from, a file its
  arguments name, the page it gave for itself, or Plug's own form. Plug
  only uses what it already holds and never searches the disk.
- Clients has an Add Client button. Pick any client (one on this Mac, one on
  the web such as ChatGPT, one Plug cannot find, or one it has never heard
  of) and how it connects, on this Mac or over the network. Plug sets up a
  client it finds with one button; for any other it shows the command or
  address to paste, alone or as a whole settings entry. A client Plug has never
  heard of can be given a name and an icon there, which it shows under from
  its first connection.
- A client whose sign-in to Plug ended says "Needs sign-in" in Clients, with
  how to sign it in again, and Plug sends a notification when it happens.
  `plug auth clients list` says so too.

### Changed

- Clients distinguish missing local configuration from hosted access. Network
  authorization details show the local HTTP clients represented by that row,
  including Codex CLI, without implying an active connection.
- A client you name after one Plug knows takes that client's icon: a second
  Cursor sign-in renamed "GrokBot" shows Grok Bot's icon, not Cursor's.
- Activity shows each call under the name you gave the client that made
  it. Two sign-ins that both report "Cursor" are told apart from then on;
  calls recorded before this stay as they were.
- Quitting Plug asks first. "Quit" closes the app and leaves Plug serving,
  with the icon away until Plug is opened again; "Turn Plug Off and Quit"
  stops it. Before, quitting left Plug serving without saying so.

### Fixed

- Two scripts started the same way from different folders, such as
  `python3 main.py`, are two clients. They were one, sharing a name and
  what they were kept from.
- A script that connects on this Mac is called by its own name, or by its
  folder when the file is a `main.py`. It showed as `python3` or `node`,
  the same as every other script.
- A task's large result is kept with the tool the task ran, so a client is
  kept from it only when it is kept from that tool. A client with a block on
  anything used to be refused every such result.
- A network client's open session ends when its sign-in runs out, not only
  when the sign-in is revoked.
- The menu bar panel and Activity show a client's icon wherever the
  Clients list does: a network client that says nothing about itself is
  pictured by its sign-in's name in the panel too, and ChatGPT is found by
  the app OpenAI ships now, whatever the owner named it.
- A tool switched off for one client is stored by its server and that
  server's own name for the tool, so renaming the tool or its server's
  prefix cannot switch it back on. Blocks written the old way, by listed
  name, are brought over the first time Plug sees the tool, and a rule
  with `*` in it now also holds for a tool that was renamed out from under
  it. A large result kept on disk follows the same block whatever the tool
  was called when it was kept.
- A client kept from a server no longer hears about that server's sign-in,
  and a local client kept from a server gets no log lines, as a remote one
  already did.
- A remote session ends when the sign-in it was opened under is revoked,
  not only when the client is removed, and nothing more is sent to it.
- A secret command's helper left holding only the error output is stopped.
- An environment entry named like another entry's stored key (`env.token`
  beside `token`) gets its own stored key.
- A watch check that was waiting its turn no longer runs after the watch is
  removed or while Plug is shutting down.
- Servers whose names read alike are numbered around names already in use,
  so a numbered name can no longer land on another server and hide its tool.
- A remote client's open session ends when its sign-in does, however that
  happened, including a removal whose save failed. A client newly kept from
  a server stops getting log lines on a stream it already had open.
- A watched tool that is slow to answer no longer delays the next check of
  the other watches, and is not called again while still answering.
- A secret command that runs past its time is stopped together with
  anything it started, and one that prints without end is cut off.
- Updated `rustls` to 0.23.45 or later for RUSTSEC-2026-0285.
- A server's bearer token and an environment entry named `token` no longer
  share one stored key, and a refused change puts each key back exactly as
  it was. A whole server change, keys included, now happens one at a time,
  so two changes at once cannot undo each other's keys.
- A watch calls a tool on a timer only when the server itself marks the tool
  read-only. A tool the server says nothing about needs `allow_writes`, as a
  writing tool does; before, a harmless-looking name was enough.
- A large result Plug set aside stays out of reach of a client kept from its
  server or tool even after the tool is renamed or the server is removed.
- Saving the config writes servers in name order, so the file no longer
  reshuffles on every save.
- Setting up a client whose config file is YAML that cannot be read leaves
  the file alone and says so, where before Plug wrote a fresh file over it.
- One watched tool that is slow to answer no longer holds up the other
  watches that are due with it.
- A secret store command that leaves a program running behind it no longer
  hangs the read; it stops at the store's time limit.
- A client kept from a tool can no longer read a large result of that tool
  that Plug set aside as a file.
- An API server's key is not sent on when the API redirects to another port
  or from HTTPS to plain HTTP on the same host. Plug also stops reading an
  API response, or an API description, once it passes the size limit, where
  before it read the whole thing first.
- Removing a client's access ends the sessions it has open, where before an
  open stream stayed up until it timed out. A session answers only to the
  client that opened it.
- A client kept from a server no longer receives the log lines servers send.
- A server change that is refused, such as adding a server under a name
  already taken, no longer replaces the key the existing server works with.
- Removing a server keeps a stored key that another account of it still
  uses; the key goes when the last one does.
- Two servers whose names differ only in punctuation, or a server with a
  very long name, no longer share one stored key. Keys already stored keep
  their names.
- Watching a tool for change follows what the server says about it: a tool
  the server marks as writing is not called on a timer because its name
  reads as harmless.
- A client that runs as several processes keeps its sign-in. Each holds
  the copy of the stored token it started with, so one renews and another
  presents the used copy later; before, that one was taken for a thief and
  the whole sign-in was revoked, which kept signing Codex CLI out. A used
  copy from the same client is now accepted for a week. After that, or from
  another client, it still revokes everything.
- A stored sign-in renews after its client restarts. A client that had not
  read Plug's addresses again posted the renewal to `/token` with no
  resource, got nothing, and asked to be approved from scratch. Scripts
  built on the MCP Python SDK failed this way an hour after signing in.
- Activity shows a network client's icon and name. A call from a client
  that says little about itself is pictured by the name on its sign-in, the
  way the Clients list already did.
- Ditto history and email-manager clients use Python icons while retaining
  their distinct workflow names.
- HTTP-linked clients no longer have a redundant local configuration row
  beside their network authorization. Existing local sessions remain visible;
  clients not using Plug have their own section.
- Clients follows links made outside the app, such as `plug link` in a
  terminal, within a few seconds. It read them once, when the tab opened.
- Running the app's tests no longer leaves a second Plug.app where Spotlight
  finds it.
- Native OAuth clients such as Claude Code can sign in using temporary
  loopback callback ports. Callback host, path, and query remain checked.
- With the sidebar hidden, the window's title and count no longer run past
  the edge of the first column.
- Linking OpenCode or Kilo Code writes the entry where those clients read
  it. It went under a key OpenCode does not accept, so OpenCode would not
  start and Plug showed it as not linked.
- Linking Claude Code over HTTP writes an entry Claude Code accepts.
- The top of each column in the window no longer changes when the pointer
  moves over it. macOS was fading a line in under the bar on hover.

## [0.10.0] - 2026-10-05

### Changed

- The menu bar icon is a plug, where it was a lightning bolt. It is solid
  when everything runs and an outline when Plug is off, and carries a small
  badge when Plug is starting, needs attention, or cannot run. The same plug
  stands for Plug in the window.
- The menu bar panel shows connected clients with the names and icons the
  Clients list gives them. A client known only by its link, or one that
  signed in over the network, showed there as a blank tile.
- The window is laid out like a Mac app. A sidebar on the left lists Servers,
  Clients, Events, and Activity. Every section is a list with the selected
  item's details beside it, where before each section had its own layout:
  cards, popovers, and rows that opened in place.
- The window has three columns: the sidebar, the list, and the selected
  item. macOS draws the bars and the dividers between them, so the top bar
  and the lines between the columns no longer look broken.
- Servers and clients show icons. A server named for an app on this Mac
  shows that app's icon, and so does a client connected over the network.
  Anything else gets a tile with its first letter. A server's state shows as
  a dot on its icon, and in the list only when it needs attention.
- The buttons at the bottom of the menu bar panel have icons again.
- An icon that arrives with an empty margin of its own is no longer shown small on a white tile.
- A server that runs as a local command, and a client with no app, find an icon too: from its maker's web site when its name carries a known one (Oura, Python, Stripe, and about eighty more), and for a server started with `npx` or `uvx`, from the site or the owner its package names. Goose and Qwen Code ship a logo.
- Servers and clients find their own icons. A server shows the icon it offers for itself, the app it runs inside, or the icon of its own web site, asked for only over HTTPS at the server's own address. Eight command line clients with no app, among them Gemini CLI, Copilot CLI, OpenCode, and Pi, show a logo that ships with Plug. Right-click any server or client and pick Choose Icon to use a picture of your own.
- Each call in Activity shows two icons, the client and the server it
  called, and a failed call is marked beside its time.
- Settings is its own window, on Command-comma and from the menu bar panel.
  It is no longer a tab in the main window.
- Lists and detail panes use the system's own list and form styles, so
  spacing, selection, and text sizes match the rest of macOS in light and
  dark.
- The app uses the same words and the same buttons everywhere. A button
  whose name ends in three dots asks before it does anything; the others act
  at once. Titles, empty lists, and error messages follow one style.
- Each client is one row, however many connections it has open. A green dot
  marks the ones connected now, and the two groups are On This Mac and Over
  the Network.
- Empty lists and problems are clearer. An empty section says what belongs
  there and offers the next step. A problem says what went wrong and what to
  do, and a call the client stopped is no longer shown as a failure.
- The menu bar panel is simpler: the headline, your servers with a fix beside
  any that need one, who is connected, recent activity, and three plain
  buttons for Open Plug, Settings, and Quit Plug.
- Settings is shorter. Open at Login, Notifications, and automatic updates
  are together at the top, and the checkup sits with the Plug switch.
- Rename a client by typing in its Name field. Leave the field empty to go
  back to the client's own name.
- Editing a server moves a key that was still written in the settings file
  to the place keys are kept, where before it stayed in the file until
  `plug secret move`.

### Added

- Muse, Meta's agent, is named and pictured when it connects as a remote
  client. It is a different client from Muse Code, the command line tool.
- Plug links four more clients: Amp, OpenClaw, LM Studio, and Muse Code.
  `plug link`, `plug import`, repair, and `plug doctor` know where each
  keeps its servers. The paths come from each maker's documentation; none
  was installed to try.
- Menus and shortcuts. File has Add Server (Command-N), Import Servers
  (Shift-Command-I), and Watch a Tool (Shift-Command-N). View has the four
  sections on Command-1 to Command-4, Refresh on Command-R, and the sidebar
  toggle. Help has How Plug Works, Run Checkup, and Show Logs in Finder. Check for
  Updates is in the Plug menu.
- A client can be kept from single tools in the app. In a client's details,
  open a server to see its tools, each with its own switch. A tool that a
  rule written with `plug clients block` turns off says so and is changed
  there.
- Choose where a server's keys are kept. The form for adding or editing a
  server has Keep Keys In, with the Keychain and Plug's `.env` file. A key
  from 1Password is used by typing where it is, such as
  `op://vault/item/field`, in place of the key.
- Delete removes the selected server, after asking. Remove Server is also in
  a server's right-click menu.

### Fixed

- `plug clients` names a remote client it does not recognise after the name
  it signed in under, as the app does. It showed as Unknown.
- Linking or unlinking a client whose settings file is not plain JSON, such
  as one with comments, no longer replaces the file with only Plug's entry.
  Plug leaves the file alone and says so.

## [0.9.0] - 2026-10-04

### Added

- Activity rows open. Click a call to see the tool, the server, the client,
  when it ran, how long it took, and whether it worked. A failed call says
  why, in the error's own words, and what to do next. The reason is kept for
  calls made after this update, cut to 240 characters.
- Add Server starts from a list. Pick Notion, Linear, Atlassian, Asana,
  Sentry, Stripe, Vercel, Cloudflare, Canva, Intercom, or Context7 and Plug
  fills in the rest and asks you to sign in; a server Plug already has is not
  offered again. Pasting a setup block, a command, or an address still works,
  one step below the list.
- A server's key can live in the Keychain instead of in `config.toml`.
  `plug secret set <name>` asks for the value without showing it, or takes it
  from a pipe, and stores it; the server then says `keychain:<name>` where the
  key used to be, as its bearer token or as the value of one of its `env`
  entries. Only the service reads the value, when it starts that server. A
  server whose secret is missing says so in `plug status` and the others start
  as usual. `plug secret rm <name>` removes one.
- A server's key can also come from 1Password, from Plug's `.env` file, or
  from any other password manager. `op://vault/item/field` asks the 1Password
  command-line tool; `file:<name>` reads the `.env` file, and
  `plug secret set --store file <name>` writes it. Any other tool is three
  lines under `[secrets.stores.<id>]`: a `command` with `{name}` where the
  secret's name goes, and `<id>:<name>` runs it. A store that locks after a
  server started does not stop that server: Plug keeps the value it read, in
  memory only, and uses it until the store answers again.
- Skills served over MCP keep their server's name. A `skill://` resource
  from a server reaches every client as `skill://<server>/…`, in the
  resource list, in reads, in update notices, and in tool results, so two
  servers with a skill of the same name no longer collide and a client can
  tell where a skill came from. A skill file that is not listed is read
  through its server's name, and a URI written without the name still works
  when one server serves it.
- A key typed into Plug goes to the Keychain on its own. Adding or editing a
  server in the app or with `plug server add` stores its token, and any `env`
  entry whose name says it is a credential, in the Keychain and writes
  `keychain:<server>.<field>` to `config.toml`. Removing the server, or the
  token, removes the stored value. A machine with no credential store keeps
  the key in the file as before.
- `plug doctor` and the app's checkup name the keys still written in
  `config.toml`, and `plug secret move` moves them all to the Keychain.
- Plug can watch a tool and tell a client when its result changes. Add an
  `[[events.watch]]` entry naming a server, a tool, and how often to check, and
  the event `<server>.<name>` appears to remote clients that support MCP
  Events. Plug only watches tools their server marks read-only unless the
  watch says `allow_writes = true`, sends nothing for the first result, and
  keeps a client away from the events of a server it is kept from. See
  `docs/events.md`.
- `plug events` lists every watch with when it was last checked, when it last
  changed, and how many clients listen. `plug events watch <server> <tool>`
  adds one and says at once when the tool does not exist or is not read-only;
  `plug events unwatch <event>` removes it.
- The app has an Events tab. It lists every event with how it is doing in one
  sentence and how many clients listen. Watch a Tool asks for a server, a
  tool, and how often to check; it offers only the tools the server marks
  read-only unless you ask for the rest, and Stop Watching removes a watch.
- The app has a first-run guide. On a Mac with no servers it opens by itself
  once; after that the question mark in the toolbar opens it. It shows Plug's
  two sides in one picture and walks three steps, add a server, connect a
  client, use a tool, each with the button that does it and a tick when it is
  done. Copy Setup Prompt puts instructions on the clipboard that an agent can
  follow to set Plug up; the same text is `docs/guides/agent-setup.md`.
- A server can have a second account. `plug server add-account <server>
  <account>` and Add Another Account in a server's menu add the same server
  again as `<server>-<account>`, with the same settings. An OAuth server's
  copy starts signed out and signs in with the other account; any other copy
  keeps the same credentials until you edit it. Tool groups carry the account
  in their name, so `Gmail` and `GmailPersonal` sit side by side.
- The Clients tab chooses which servers each client can use. Every client that
  uses Plug has a button that opens its servers with a switch each, and a
  client kept from something says so in its row. The popover says plainly that
  for a client on this Mac this keeps the list short and is not a lock, while a
  remote client is held to it.
- A client kept from a server stops hearing that server's resource updates,
  even for a subscription it made before the block. Lifting the block brings
  them back.
- A client kept from a server is kept from all of it. Its resources, resource
  templates, and prompts leave that client's lists, and reading one, getting
  one, completing against one, or subscribing to one answers as if it did not
  exist. Before, a block covered the server's tools only.
- Pi, Warp, and Kiro are clients. `plug link pi`, `plug link warp`, and
  `plug link kiro` write each one's own MCP file, the Clients tab lists them,
  and `plug import` reads servers from them.
- GitHub Copilot CLI is a client. `plug link copilot-cli` and the Clients tab
  write `~/.copilot/mcp-config.json` with the `type` and `tools` fields Copilot
  CLI requires, and `plug import copilot-cli` reads servers from it.
- Every request knows which client sent it. `plug connect --client <target>`
  says which link started the connector, so a client is placed by how it was
  linked rather than by the name it reports; a remote request is placed by its
  grant, and requests on the shared token share one key. Nothing acts on the
  key yet: it is the ground per-client access stands on.
- `plug link` and the Clients tab write `connect --client <target>` into a
  local link, so a client whose own name Plug does not recognise (Pi, Warp,
  Kiro, Kilo Code) shows under its real name and icon. Links written before
  this keep working unchanged; link the client again to pick it up.
- A client can be kept from a server or from single tools. Under
  `[clients."<key>"]` in the config, `blocked_servers = ["slack"]` and
  `blocked_tools = ["github__delete_*"]` take those tools out of that client's
  list and make a call to one answer as an unknown tool, by any route: a
  direct call, a task, tool search, or the invoke wrapper. The key is the one
  `plug clients -v` shows. A change applies on config reload, and connected
  clients are told their tool list changed. For a local client this keeps a
  tidy tool list, it is not a security boundary: any program running as you
  can link itself under another name. For a remote client the key is its
  verified grant.
- `plug clients block <client> --server <name>` and `--tool <name>` keep a
  client from a server or a tool, and `plug clients unblock` lets it back in.
  The client is picked as in `plug clients rename`: by the name it shows
  under, or by its key. It applies at once, without a reload, and
  `plug clients` lists who is kept from what.
- An unknown local client started by an interpreter is told apart by the script
  it runs. Two tools that both run under `python3` or `node` are now two
  clients, each with its own name. A name given to such a client before this
  change no longer applies; rename it once more.

- A remote session knows the grant it came in on. Clients names a remote
  client Plug does not recognise after that grant, shows the matching app's
  icon when the Mac has one, and lets it be renamed from its own row; the name
  is stored under the grant, so it follows the client across sessions.
  `plug clients -v` lists the session's key as `oauth:<client id>`.
- A client can be renamed. Clients has a pencil on every remote client and on
  every local client Plug can tell apart, and `plug clients rename <client>
  "<name>"` does the same; an empty name goes back to the one Plug works out.
  The name is kept in `[clients."<key>"]` in the config, under the client's
  target, the program that started it, or its grant, never under the name it
  reports. `plug clients -v` lists each key.
- A local client Plug does not recognise is named after the program that
  started it. `plug connect` reads its parent from the process table, looking
  past shells and launchers, and reports it when it registers; Clients shows
  that app's name and icon in place of "Unidentified local client", and
  `plug clients` lists it under that name. A client cannot choose this name,
  unlike the one it sends in `initialize`.
- Opt-in Slack event delivery. With `[http.slack_events]` configured, Plug
  receives Slack's Events API at `/events/slack`, keeps coworker mentions of one
  Slack user and replies in those threads across public channels, and delivers
  each as `slack.ditto_message` to one chosen downstream OAuth client through
  modern MCP Events (`events/list`, `events/subscribe`, `events/unsubscribe`,
  scope `events:subscribe`). Callbacks are verified and signed, subscriptions
  and the retry queue survive restarts, and the Slack signing secret is entered
  at a hidden prompt (`plug auth slack-events set`) and kept in the Keychain.
  Off unless configured; Slack only. See `docs/slack-mcp-events.md`.
- `owner_proof_until` lets the owner prove Slack event delivery alone for up to
  one hour by sending one exact test marker. Every other owner message stays
  excluded.
- Grok Build is a client. `plug link grok-build` writes `[mcp_servers.plug]` to
  `~/.grok/config.toml` (or `.grok/config.toml` with `--project`), import reads
  servers from it, and a Grok Build session is named in Clients. Grok Bot,
  which reaches Plug over the internet only, is recognised by name and shows
  its icon when the Grok Bot app is installed.
- Plug links Hermes Agent. `plug link hermes` adds a `plug` entry under
  `mcp_servers` in `~/.hermes/config.yaml` and changes no other line of the
  file. A link written there by hand, `Plug` or `plug`, is recognised and
  replaced. `plug import hermes` reads its servers.
- The app shows Warp, Kiro, and Hermes Agent with their own icons, and names
  Gemini, Perplexity, Le Chat, and Poke when they connect.
- Plug links Kimi Code and Qwen Code. `plug link kimi-code` writes
  `~/.kimi-code/mcp.json` and `plug link qwen-code` writes
  `~/.qwen/settings.json`; both also import from those files and show in the
  app's client list.
- An HTTP API can be a server. `plug server add <name> --openapi <url-or-file>`
  reads an OpenAPI 3 document, JSON or YAML, and lists each operation as one
  tool; a call makes the HTTP request. `--url` sets where the API lives when
  the document does not say, `--bearer-token` signs in, and `--operations`
  picks which operations to expose, which an API with more than 50 requires.
  GET and HEAD operations are marked read-only, a status of 400 or above is a
  tool error, and requests go only to the API's own address. In the config
  this is `transport = "openapi"` with `spec` and `operations`.
- The app adds an HTTP API as a server. Paste the address or the file path of
  an OpenAPI document into Add Server; Plug guesses whether an address is a
  server or an API document, and a picker corrects the guess.
- An API server can sign in with an API key. When the OpenAPI document says
  the key goes in a header or a query parameter, the token from
  `--bearer-token` is sent there; `--token-in header:<name>`, `query:<name>`,
  or `bearer` (`token_in` in the config) says so when the document does not.
- Add Server lists an API's operations, grouped as the document groups them,
  with a checkbox on each. An API with more than 50 operations starts with
  none chosen and is added once 50 or fewer are.

### Changed

- Plug has a new icon: a white power symbol on blue. It shows in the Dock, in
  Finder, and inside clients that picture the servers they connect to.
- Clients shows one row and one switch for each client. Press a row to see
  which servers the client can use, what is connected now, and to rename it.
  Sessions, ids, and Revoke are no longer on the page: turning off a client
  that connects over the network asks first, then removes its access.
- The menu bar panel and the window now agree. Both show the same count line
  ("12 servers · 140 tools"), the panel lists servers that are off, dimmed and
  last, and a server row says "14 tools" in both places.
- Every sheet has the same width, the same footer, and the same way of saying
  what went wrong, including Add Another Account and Watch a Tool. Switches in
  rows are one size.
- A problem says what happened and what to do. A press that fails now reads
  "Could not restart notion.", then the reason, then the next step, and it
  stays until you dismiss it; it used to be the raw error and leave after 8
  seconds. The window, the menu bar panel, and every sheet show a problem the
  same way.
- An import that adds some servers and not others names the ones it could not
  add, with the reason under each, and leaves only those ticked so trying
  again retries only what failed.
- A call's duration reads the same in Activity, the menu bar panel, and a
  server's recent calls. A caller Plug cannot name is "Unknown client".
- Adding and editing a server are one form. After a pick or a paste, Add
  Server shows the same fields Edit does, filled in, so what Plug understood
  can be read and corrected before it is saved. The fields are Address and
  Key, or Command and Arguments, and Settings; the form says a key is kept in
  the Keychain. When Edit cannot read a server's settings it says why and
  offers Try Again, where it used to show an empty sheet.
- The command line uses the app's words. `plug status` leads with whether
  Plug is running, how many clients are connected, and one line per server
  (Running, Unsteady, Down, Sign-in needed; On this Mac or Over the network);
  the connection and address detail moved behind `plug status -v`.
  `plug doctor` names each check the way the app's Checkup does, and the app
  now takes those names from the same list. `plug clients` says Set up, On
  this Mac, and Connected now, and keeps link and lazy-tool detail for `-v`.
- `plug server on|off` and `plug tools on|off` replace `enable|disable`; the
  old words still work. `plug --help` no longer lists the commands only Plug
  itself runs (`connect`, `serve`, `stop`, `reload`); they still work.
- Settings is a section of the Plug window, next to Servers, Clients, Events,
  and Activity, as one page: the switch that turns Plug off, Restart, Checkup,
  the three preferences, the settings file and logs, and the version. The
  separate Settings window is gone, and the gear in the menu bar panel opens
  this section. When Plug is stopped or several servers need attention, the
  banner offers Checkup, which opens Settings and runs it.
- The app uses one word for one thing. A server is "On this Mac" or "Over the
  network", the same as in `plug status`. The file is always the "settings
  file". Whatever uses Plug is a "client", never an "app". A tool locked by a
  rule is "Off by rule" everywhere, the list of calls is "Recent calls"
  everywhere, importing is "Import Servers…" everywhere, and the guide has one
  title, "How Plug works". A server Plug has not started yet offers "Load It".
- Devin replaces Windsurf everywhere Plug speaks: the target is `devin`
  (`windsurf` still works as an alias), it writes
  `~/.config/devin/mcp_config.json`, the file Devin reads today, and import
  also scans the old `~/.codeium/windsurf/mcp_config.json`. `plug doctor` no
  longer warns that a linked Devin accepts only 100 tools; that ceiling is
  Cascade's, the desktop app's legacy agent, and still applies to a session
  that introduces itself as `windsurf-client`.
- The window's Apps tab is now Clients, and every program that uses tools
  through Plug is called a client across the app, the README, and the docs.
  Agents, apps, and scripts are all clients; `plug clients` already said so.
  The vision doc (`docs/VISION.md`) now carries the five words Plug uses and
  the words it avoids.
- Docs match the shipped product: the README describes the app, the
  architecture and dual-era guides describe `main` instead of a development
  branch, and the August app design review moved to `docs/archive/`.

### Fixed

- A server that is off stays off. Editing its settings, moving its key to
  the Keychain, or turning it off used to start it again until the next
  restart of the service, and Restart in the app started it too.
- `plug server add` with flags no longer stops to ask for arguments when the
  command takes none.
- MCP Apps work through Plug. Plug now tells each server it can show app
  pages, so servers that check before offering one offer it; the `ui` field
  that links a tool to its page and carries the page's sandbox settings is
  passed on instead of dropped; and a page can call its server's tools by the
  names that server gave them, as long as only one listed tool has that name.
- A removed server that never connected, one waiting for sign-in or one that
  failed to start, stayed in `plug servers` and the app until the service
  restarted. It now leaves at once.
- Linking VS Code wrote a file VS Code does not read, in a shape it does not
  accept. The link now writes top-level `servers` to `mcp.json` in the VS Code
  user folder, or to `.vscode/mcp.json` for a project link. Link VS Code again
  to pick this up; the old `mcp` entry in `~/.copilot/mcp-config.json` is
  ignored by both programs and can be deleted.

## [0.8.13] - 2026-10-01

### Changed

- The menu bar panel has a Plug on/off switch, separate from Quit. Turning
  off confirms that connected apps lose access, stops the app-owned background
  service, and stays off across launches. Turning on restores the service.
  Quit only closes the menu bar app and leaves a running service alone.
  Recent-call durations keep their full width beside long tool names.
- Apps uses the current session list for connected state and counts. A stale
  app scan no longer invents a connected app or keeps an old session count.

- The window has three sections: Servers, Apps, and Activity. Tools moved
  into the server they belong to: Servers shows the list on the left and the
  selected server in full on the right, with its tools and their switches.
  Searching in Servers finds tools as well as servers, and clicking a tool
  shows its details beside it. A server's header carries Restart, Edit, and
  an on/off switch. Connections is now called Apps.
- Apps groups by what matters first: Connected now, On this Mac, and Remote
  clients. A connected app opens to show each of its sessions with its id,
  how long it has been open, and how many tools it can reach. A remote
  client shows the site it signs in from, or a short id, in place of its
  registration method.
- The menu bar panel lists the three most recent tool calls, with the server,
  the outcome, and how long each took. Clicking them opens Activity.

## [0.8.12] - 2026-10-01

### Changed

- `plug doctor` can come back clean. With downstream OAuth it now fetches the
  OAuth metadata from the public URL, passing when the tunnel serves it and
  warning with the error when it does not, instead of always warning that it
  did not check. The daemon line reads as a sentence, and the port and PID
  checks that only repeated "daemon running" fold into it. A new check warns
  when a remote app has registered more than once, and `plug auth clients
  list` shows when each registration was made, last used, and expires. Every
  "restart Plug" hint now points to Plug.app's Restart button when the app is
  installed, and `plug stop` answers with a sentence.
- A failed server now says why. `plug status`, `plug servers`, `plug doctor`
  and the app show the error from its last start or reconnect, including the
  last line a local server printed before it exited, with its secrets
  redacted. `plug doctor` also warns when a server's command is found only
  through your login shell PATH, which the daemon cannot always read at boot.
  The daemon no longer logs a stdio server's arguments, which can hold tokens.
- `plug tools` opens with one row per server and its tool count; `plug tools
  <server>` (or a group such as `gmail`) lists that server's tools, and `plug
  tools -v` lists them all. `plug clients` groups live sessions by client with
  a count (each session behind `-v`), lists linked clients first, and folds
  the unlinked ones into one line. Durations read as `3h 5m` or `59d` instead
  of raw seconds, labels line up, `plug servers` shows just the protocol
  version (prefixed `modern` when it is), the always-zero `sse=0` count is
  gone, and every subcommand and flag in `--help` has a description. The
  count of `disabled_tools` rules shows only in the full `plug tools` list,
  since it applies to every server. JSON output is unchanged.
- Node-based stdio servers such as Figma no longer stay down after a reboot.
  When the login shell was too slow to report its PATH at boot, Plug kept that
  failure until the daemon restarted, so `node` and `npx` could not be found.
  It now retries the lookup and falls back to the Homebrew directories
  meanwhile.
- Stdio servers start about five seconds sooner after a reboot. Plug saves the
  PATH its last login-shell lookup found and uses it at once, refreshing it in
  the background, instead of waiting on a login shell that is still slow.
- `scripts/perf.sh` measures the journeys Plug owns against the installed
  app: connector startup, tools/list, ping and call overhead, and from the
  logs, per-server call latency, daemon startup, and the daemon swap gap.
- A new `plug connect` is ready about 100ms sooner. When it runs from inside
  Plug.app it no longer re-verifies the app it is running from, which cost a
  codesign check and two subprocesses per connector.
- Updating Plug no longer fails a tool call that is running. On shutdown the
  daemon waits up to eight seconds for running calls to finish before it
  disconnects clients and stops servers.
- A daemon swap takes about a second less. A stdio server that stays up after
  its stdin closes, like Figma's, now gets SIGTERM after a quarter second
  instead of holding shutdown for the full timeout, and the "upstream
  shutdown timed out" warnings it caused are gone.
- The daemon keeps its last 14 daily log files and deletes older ones. It
  used to keep them forever.
- A daemon swap now closes HTTP servers properly. Disconnecting clients were
  still notifying them when shutdown reached them, so shutdown gave up on
  closing each one and logged "could not take ownership of upstream". It now
  waits up to a second for that notification to let go.
- Quieter logs and status. A connector that asks for an older MCP version no
  longer logs a fallback warning, the connect log reports the version actually
  negotiated, and `plug auth status` shows a disabled OAuth server as
  "disabled" instead of warning about its stale credentials.
- A config.toml that does not parse, such as one saved mid-edit, no longer
  blanks Plug.app's status. The daemon keeps serving its last good config, the
  status reports the parse error, and each status poll reads the file once
  instead of twice.
- Plug.app no longer says "Plug is not running" after an update or restart.
  It reconnects to the new background service on the same read, shows
  "Reconnecting…" while a dropped connection comes back, and only calls Plug
  stopped after ten seconds. Settings → Restart shows "Restarting…" and a
  Start Plug pressed meanwhile waits for it instead of starting a second
  swap. A stopped Plug no longer flickers to "Starting…" on every poll.
- A failed upstream connection is logged once instead of three times. The
  daemon log no longer repeats RMCP's "worker quit with fatal" line or a
  second "server initialization failed" line beside Plug's own. The warning
  that `max_concurrent > 1` is unsafe for stdio servers is gone, since it was
  wrong and fired on every config load. Commands like `plug doctor` and
  `plug config check` no longer print log warnings above their output.
- A failed button press in Plug.app stays on screen until you dismiss it or
  eight seconds pass, in the menu bar panel, the window, and under the
  Settings buttons. It used to vanish at the next poll and showed only while
  every server was healthy. A sign-in in progress offers Try Again and
  Cancel, and both stop the waiting `plug auth login`. Clicking a "needs
  sign-in" notification opens that server, and its Sign In button starts the
  sign-in.
- A server that stays down is retried with growing backoff, up to once a
  minute, and quietly. Recovery used to restart at a one-second delay on every
  health check and retry inside each attempt, logging a warning or error each
  time. In an hour of a missing command, start attempts drop from 312 to about
  60 and log lines from 312 to 3, and recovery logs one line saying how many
  attempts it took.
- Plug.app polish. It shows the running daemon about four seconds sooner
  at launch. The menu bar panel dims its server rows and says "Last known"
  when they are stale. A server the daemon has not loaded a minute after it
  started reads "Not loaded" with a Reload button, not "Starting" forever.
  Only starting servers pulse. VoiceOver can reach the fix buttons in panel
  rows and the Use Plug switch in Connections. About shows the app's version
  when the daemon is not answering.

- The menu bar panel is redesigned. A larger tinted headline says whether Plug
  is working and carries its fix, a thin progress line shows servers settling
  after launch, every enabled server stays in the list with its tool count,
  troubled rows keep their place and carry an inline Restart or Sign In
  button, and a row of connected app icons shows who is using Plug. Add
  Server, Open Plug, Settings, and Quit sit in a flat footer.
- The main window is tighter. Search lives in the toolbar as a native search
  field, list rows are denser, tool rows highlight on hover, and Activity
  rows show one app icon, the server and tool name split apart, which app
  made the call, and turn the latency orange when a call took five seconds or
  more.
- The version in About can be selected and copied.
- Downstream OAuth state from before 0.4.0 (`issuer-v2-*.json`) is no
  longer migrated at startup, and the one-time grant scope and token-family
  backfills are gone. State written by 0.4.0 or later loads unchanged.
- Downstream OAuth state writes no longer stall other requests on the same
  daemon worker thread, and the state file is written as compact JSON.
- Servers use the same words everywhere: the detail header says "Running · N
  tools" or "Off" like the rows do, a degraded server sorts with the working
  ones instead of above them, and the notification setting now says what it
  does: "Tell me when a server needs sign-in or a new app connects".
- `plug codesign-setup` is gone. It only ran for a `PLUG_DEV=1 plug-dev`
  binary, a development path `dev-install.sh` already replaced.
- A `plug connect` client can now have many requests in flight at once, so
  one slow tool call no longer holds up that client's other requests. IPC
  protocol version is now 4. The daemon still serves v3 clients one request
  at a time, so running `plug connect` processes keep working after an
  upgrade without a host restart.

### Removed

- `install.sh` is gone. It exited on macOS and the Linux tarballs it
  downloaded stopped at 0.8.10. Install Plug.app from the DMG or the
  `plug-app` Homebrew cask.

### Fixed

- Tool calls no longer fail in Claude Code with "missing required resultType".
  A local client that connected through `plug connect` on protocol `2026-07-28`
  got every proxied tool result without that field and rejected it. The
  connector now completes it on those sessions, including when the client opens
  with a plain `initialize` that negotiates `2026-07-28`. Older clients are
  unchanged.
- Remote OAuth clients such as ChatGPT can connect when their client metadata
  lists `none` among supported authentication methods, even when their legacy
  preference is `private_key_jwt`. Older metadata and dynamic registration keep
  their existing checks. Unsupported token assertions and authorization headers
  are rejected instead of being silently ignored; PKCE and owner consent remain
  required.
- Updating Plug.app no longer leaves about twelve seconds without a daemon.
  The replacement killed the daemon registration had just started, and
  launchd's respawn throttle held the restart back; the gap is now under two
  seconds.

- `plug connect` now exits when its host closes stdin. Before, it kept
  running with a live daemon session after the app or agent that started it
  was gone.

- Toggling a tool or server from Plug.app or `plug servers` no longer
  restarts every server whose config uses `$VAR` env references. The reload
  compared the raw file against the expanded running config, saw every
  `$VAR` as changed, and restarted those servers with the literal
  placeholders instead of their values.
- A login shell that hangs while Plug reads its PATH no longer stalls stdio
  server starts. The probe now runs off the async workers and gives up after
  five seconds, falling back to the inherited PATH.
- Opening Plug.app no longer stops a Homebrew-installed daemon or uninstalls
  the formula before you approve adoption. Both now wait for the same consent
  as taking over the daemon.
- Right after login the menu bar no longer shows "Setup incomplete" with the
  detail "timedOut". A command that times out while the Mac is still busy now
  waits five seconds and runs again, up to six times, before Plug reports a
  problem, and the report says what timed out in plain words. The checks that
  run at launch also get more time: reading the bundled command's version
  allows fifteen seconds instead of three, asking launchd about each job
  allows fifteen instead of five, and a slow answer about someone else's job
  is skipped rather than treated as a failure.
- Show Log on the setup banner opens a real file. Reconciliation now writes
  `~/Library/Logs/Plug/installation-reconciliation.log`, one timestamped line
  per step, so a failed setup can be read after the fact.
- Starting an OAuth HTTP upstream no longer rediscovers authorization-server
  metadata when a verified bound token for that resource already exists. A
  hung well-known GET used to burn the start budget, then recovery paid the
  same walk again on every retry. Restart and reconnect now share one
  in-flight start so they cannot pile two handshakes on the same server.
- The documented macOS config path is now
  `~/Library/Application Support/plug/config.toml`. README and the operator
  guide had been pointing at `~/.config/plug/`, which is the Linux location.
- The passkey consent page now says what `logging:read` and
  `continuations:complete` allow instead of the generic "Use the requested
  Plug capability".
- A server the daemon reports as degraded now shows as running in the app
  instead of "Unknown" with a Restart button.
- The app reads responses the daemon splits into chunks, so a server list or
  tool list larger than 4 MiB no longer fails to load.
- OAuth servers that start at the same moment no longer race the Keychain
  store setup. The loser read its Keychain copy as missing, logged "incomplete
  issuer-bound credential mirror rejected", and rediscovered OAuth metadata.
- The OAuth client-metadata fetch now stops reading at its 64 KiB cap even
  when the response has no `Content-Length`. A chunked body used to be
  buffered in full for up to five seconds.
- A remote client that cancels an elicitation or sampling request no longer
  leaves the pending request behind. It used to stay registered, and replay
  to the client on its next reconnect, until the session ended.
- A remote HTTP session that sends a request just as the idle sweep runs no
  longer loses its subscriptions, roots, and tasks while it stays open. The
  sweep now rechecks expiry at the moment it removes a session.
- Plug.app no longer leaks a socket each time it checks on the background
  service. Setup and repair checked several times per pass, and up to 180
  times while a new service started, without closing the connection.
- The menu bar panel and main window update every two seconds from the moment
  they open. The background poll used to finish its 30-second sleep first, so
  after the first read an open panel could miss changes for up to half a
  minute.
- After you switch a tool or server, sign in or out, or connect an app,
  Plug.app shows the result without waiting for the next poll. A refresh
  asked for while another was running used to be dropped, so the view could
  keep showing the state from before the change.
- Plug.app no longer reloads the full tool list, about a megabyte, after
  every action. It reloads it when the daemon reports the list changed, or
  when you choose Refresh.
- Plug.app no longer runs `brew` two or three times on every launch. It asks
  Homebrew about the old formula only when a `plug` keg is on disk under
  `/opt/homebrew` or `/usr/local`; each call took about a second, longer at
  login.
- Plug.app launches with far fewer checks when nothing needs setting up: it
  no longer inspects the app, the command, the clients and the daemon a
  second time after a pass that changed nothing, and it trusts the launchd
  inspection it just made, so the first refresh comes sooner.
- The "Show Plug in the menu bar at login" toggle starts from what macOS
  reports instead of a value Plug remembered separately.
- A tool result or schema with a key named `envelope` no longer fails the
  whole call through `plug connect` with "invalid envelope message".
- Through `plug connect`, an upstream error from reading a resource, getting
  a prompt, completing, or listing now reaches the client as that error (for
  example "resource not found") instead of "failed to parse".
- Sending a client's roots to the daemon through `plug connect` no longer
  leaves its reply behind for the next call to read when the daemon pushed a
  notification first, and a wedged daemon during that send now trips the
  read watchdog instead of hanging.
- The daemon no longer wakes ten times a second for every connected
  `plug connect` client to check whether the modern protocol gate changed.
  Gate changes are now pushed the moment they happen.
- The operator snapshot's visible tool count for a lazy-bridge session now
  includes the tools that session loaded, instead of counting only the meta
  tools.
- A server that failed at startup and later recovered now gets its
  `max_concurrent` limit and circuit breaker. It used to run with neither
  for the rest of the daemon's life.
- One upstream that never answers `logging/setLevel` no longer holds up
  every server that finishes starting after it. The log level is now pushed
  in the background with a five-second bound, and also after a reload or
  reconnect, which used to skip it.
- A legacy server that ignores `tasks/list` now starts. The task capability
  probe waited up to the 300-second call timeout inside the 30-second start
  timeout; it now gives up after three seconds and runs beside the tool list.
- Re-listing tools after an upstream's `tools/list_changed` now times out
  after the server's `call_timeout_secs` instead of waiting forever.
- A server that failed at startup is now retried the moment its health task
  starts. It used to sit out a random pause of up to ten seconds meant only
  to stagger pings to healthy servers.
- `plug auth inject` for an OAuth server with no earlier login now works.
  It saved the token without binding it to the server's authorization
  server, the runtime refused it, and the server stayed "auth required". It
  now discovers and binds the authority first, as `plug auth login` does.
- `plug doctor` no longer warns about the OAuth token file that every
  signed-in server keeps. It now warns only when another user can read that
  file, and suggests `chmod 600`.
- `plug doctor` reports a missing stdio server program once, under server
  programs, instead of also failing connectivity for the same server.
- With `tool_filter_enabled = false`, a client whose lazy-tools setting is
  `standard` or `native` now gets every tool even when the global setting
  (or `meta_tool_mode`) hides tools behind search. It used to get the
  global setting's search-only list.

## [0.8.11] - 2026-09-01

### Added

- `scripts/dev-install.sh` builds Plug.app from the working tree, signs it with
  the Developer ID already in the login keychain, and installs it in place.
  About two minutes cold and seconds warm, with no network, notarization,
  version bump, or commit. It stamps the current time as the build number so
  the app replaces its own daemon and Sparkle never offers a downgrade. This is
  the loop for trying a change on this Mac; releases stay on `release.sh`.

### Changed

- The development gate is lighter. Five CI jobs that gated nothing for a
  single-developer tool (MSRV check, cargo deny, a macOS duplicate of the
  Linux tests, a cross-compile check, and a binary-size ceiling) are gone,
  along with the disk-space guard that ran on every commit, checkout, merge,
  and push. The app test lane builds once instead of twice. Ten scripts that
  nothing ran, seven of them tests of other scripts, are deleted, and the
  old `plug-dev` path (`dev-reinstall.sh`, `setup-codesigning.sh`) retires in
  favor of `dev-install.sh`. `plug doctor` points at the new loop.
- Releases ship the macOS app only. The four Linux tarballs, the cargo-dist
  shell installer, the source tarball, and the `plug` Homebrew Formula are no
  longer built; Linux users build from source with `cargo install`, and the
  tap's formula stays at 0.8.10. GitHub release notes are now the matching
  `CHANGELOG.md` section instead of a `git-cliff` rendering of commit
  subjects. The release build restores its Rust cache from a job that runs
  on every push to `main`, where before it compiled cold on every tag.
- `scripts/ship.sh` returns to `main` after arming auto-merge and steps off a
  ship branch whose pull request already merged, so a follow-up change can no
  longer be pushed onto a dead branch. `scripts/release.sh` then waits until
  that prepare pull request actually lands before tagging, instead of treating
  the return to `main` as proof the version bump is already there.
- The repository's agent and contributor instructions collapse into one short
  `CLAUDE.md`; `AGENTS.md` points at it. The project-state snapshot, plan,
  truth rules, workflow model, per-change doc-update checklists, finished
  todos, plans, brainstorms, research, solution write-ups, audits, and
  per-version release-notes files move under `docs/archive/`. Open work now
  lives in `docs/STATUS.md`; `CHANGELOG.md` is the only change record.
- Each app section now opens with one page header that carries its title, a
  live summary, and the section's controls, so the window toolbar holds only
  search. Loading and unavailable states share one look, and every section has
  a retry.
- The menu bar panel closes itself before opening a window, sheet, or
  inspector, so it no longer floats above what it just opened. Its attention
  list and footer sit in glass containers on macOS 26, animations respect
  Reduce Motion, and troubled servers no longer count twice.
- Accessibility labels, header traits, and named actions cover the tool list,
  server list, connections, settings, and the menu bar panel.
- `plug clients` lists Devin under its current name while keeping the Windsurf
  config target it still reads, and no longer lists RooCode.

## [0.8.10] - 2026-08-31

### Changed

- Restored the compact, glass-forward macOS interface shipped by the 5:36 PM
  0.8.8 development build. The centered section control, compact window,
  independent glass actions, original inspectors, settings layout, and menu bar
  panel replace the later sidebar redesign while retaining notification consent,
  accurate service and restart feedback, unique app counts, and single-instance
  launching.

## [0.8.9] - 2026-08-31

### Fixed

- Tool calls over HTTP no longer fail for every server once modern downstream is
  enabled. Revision `2026-07-28` makes `resultType` mandatory on every result and
  limits the absent-means-complete bridge to servers on earlier revisions. Plug
  advertises that revision downstream while deliberately negotiating `2025-11-25`
  with its upstreams, so a proxied result reached the client with the field
  missing and strict clients rejected the whole response, taking down every
  configured server at once rather than one. The HTTP JSON and SSE seams now mark
  a proxied result complete, which is the meaning the bridge would have given it.
  Legacy sessions keep their existing wire shape.

- Opening Plug on a healthy Mac no longer claims it is installing itself. Every
  launch begins by reading the installation, and that read-only pass was sharing
  its words with the phases that actually change it, so the menu bar greeted a
  working install with "Setting up… Finishing installation." It now says
  "Starting… Connecting to servers.", which is what is happening; only the
  phases that repair, replace, or clean up call themselves setup.
- A repair in progress describes itself instead of the situation it started
  from. The app kept a second copy of the installation state and refreshed it
  only when reconciliation finished, with a timer to reveal a generic notice
  while the copy was stale. Reading the coordinator's state directly retires
  both the copy and the timer.

## [0.8.8] - 2026-08-31

### Fixed

- Upgrading the daemon no longer strands a running `plug connect`. The IPC
  handshake refuses a mixed-version session, which is right, but a client that
  was already running when the daemon was replaced kept answering every call
  with `daemon reconnect failed: daemon/client version mismatch` forever, and no
  MCP host recovers from that on its own. A running process cannot re-exec
  itself into a newer binary, so the client now exits when it sees that
  mismatch on reconnect and lets the host spawn a fresh one from the installed
  binary. It exits only when the daemon is running the client's own executable
  file, which is exactly the case a respawn fixes; a client installed against a
  different copy of plug reports the mismatch instead, with a message saying a
  restart will not help, rather than exiting into a loop. A mismatch on the very
  first handshake still fails outright, since a process spawned moments ago is
  not the stale one.

## [0.8.7] - 2026-08-31

### Fixed

- The downstream OAuth issuer state lock now waits briefly before declaring a
  second writer. The lock is what keeps two processes from racing state
  publication, but a daemon restarted the moment its predecessor exits could
  find the lock still held by a descriptor the old process had not finished
  closing, and fail startup with an error that only a second restart cleared.
  Acquisition now retries for up to a quarter second; a genuinely live writer
  holds the lock far longer than that, so the guard keeps its meaning, and an
  I/O error still fails immediately rather than waiting out the retries. This
  also addresses a rare Linux CI failure in
  `revoke_sync_failure_degrades_lifecycle_and_restart_stays_revoked`, whose
  exact mechanism was never reproduced on macOS.

## [0.8.6] - 2026-08-31

### Fixed

- `plug doctor`'s `client_limits` check no longer guesses a tool count. It used
  to multiply the enabled-server count by an assumed ten tools each and warn
  whenever that product cleared a published client ceiling, which is wrong in
  both directions: thirteen small servers invented 130 tools and warned, while
  four large ones could hide a real 600 and pass. Doctor never starts a server,
  so it cannot know the total; what it can read from disk is which clients are
  pointed at plug. The check now reports a ceiling only for a client that is
  actually linked, and points at `plug status` for the real count. Windsurf (100)
  and VS Code Copilot (128) were both re-verified against their documentation on
  2026-08-30 and still stand.
- `plug doctor` names the 100-tool ceiling after the product that publishes it:
  Cognition acquired Windsurf, and the cap belongs to Cascade, now the legacy
  agent inside Devin Desktop. Devin Local, the agent that replaced it as the
  default, configures MCP through the Devin CLI and publishes no ceiling. The
  export target keeps the Windsurf name because the file it writes still does.
- The daemon no longer sends upstream credentials back over the IPC socket.
  `GetServerConfig` returned the stored `ServerConfig` whole, so every operator
  client read each upstream's bearer token and full environment in plaintext.
  Secrets are now replaced with a placeholder on the way out and restored from
  the stored config on the way back in, so editing a server in Plug.app keeps
  credentials it never received. `oauth_client_id` and `oauth_scopes` stay in
  the clear: a client ID is a public identifier and scopes are names, and the
  OAuth secret lives in the credential store, not in `ServerConfig`.

### Changed

- The leftover-path classify table is now pinned by
  `testdata/legacy_plug_programs.json` itself, not only its cases. Rust's
  `is_recognized_legacy_program` and Plug.app's `LegacyPlugProgram` each assert
  their table against the fixture, so a path shape added to one language without
  the other fails a test on both sides.
- `plug doctor` and uninstall cleanup no longer treat every launchd label ending
  in `.plug` as plug's own. `local.claude-rc.plug` runs `claude` with this repo
  as its working directory and was reported as an unknown plug job on every
  doctor run. A job qualifies now by sitting in the `com.plug.` label namespace
  or by running a binary named `plug`.

## [0.8.5] - 2026-08-30

### Fixed

- Each upstream's health task now starts as soon as that upstream's own start
  attempt settles, rather than after every upstream has settled. One slow server
  used to hold back every other server's first health tick: measured after a
  reboot, `imcp` took 55.7s to spawn, and two loopback upstreams that were ready
  and reachable 44 seconds earlier went unnoticed for that whole stretch.
  `ServerManager::start_all` reports each server as it resolves, and the bulk
  spawn that follows now only covers servers it never heard about.

## [0.8.4] - 2026-08-30

### Fixed

- An upstream that failed to start is now retried as soon as its health task
  begins instead of one full `health_check_interval_secs` later. The task
  consumed the interval's immediate first tick unconditionally, so a local
  upstream that was merely slow to bind its port after a reboot stayed down for
  the whole first interval even once it was ready: on a 60s interval this was
  measured at 122 seconds of avoidable downtime for two loopback servers at
  login. A server that started healthy still skips that first tick, since it
  was just contacted.
- `scripts/install-release.sh` now swaps the app bundle by rename instead of
  deleting it in place. Every MCP client's `plug connect` re-execs the bundle's
  binary, and macOS aborts a running process whose signed executable is
  unlinked underneath it, so installing 0.8.3 killed six live client processes.
  The superseded bundle waits outside `/Applications` until its last clients
  exit, and is swept on the next install.
- The release workflow moved to `actions/upload-artifact@v6` and
  `actions/download-artifact@v7`, the first majors that actually default to
  Node 24. The v5 majors advertised Node 24 support but still declared
  `runs.using: node20`, so the deprecation warning survived the first bump.

## [0.8.3] - 2026-08-29

### Fixed

- Plug.app no longer crashes while reconciling its install. ServiceManagement
  delivers the `SMAppService.unregister` reply on a background XPC queue, and
  the completion handler was written inside a `@MainActor` type, so the Swift 6
  toolchain the release runner uses treated it as main-actor isolated and
  trapped on an executor check the moment the reply arrived. The crash left the
  daemon unregistered and took every connected client down with it. The handler
  is now explicitly `@Sendable`, which is what the callback contract has always
  been.

### Changed

- `scripts/release.sh` takes a finished change all the way to a release
  installed on this Mac: version bump, changelog heading, pull request,
  auto-merge, tag, release build, signed install, and the build-cache sweep.
  `scripts/install-release.sh` does the install half on its own, verifying the
  DMG against the release checksums and refusing anything Gatekeeper rejects.
- CI and release workflows moved to the artifact, cache, and Node actions that
  run on Node 24. The Node 20 versions are deprecated and were already being
  forced onto a newer runtime.

## [0.8.2] - 2026-08-29

### Fixed

- The CLI now allows a cold daemon start the same 90 seconds Plug.app allows,
  and waits instead of force-restarting when another process already holds the
  daemon's runtime lock. Both halves drive one daemon; the CLI's previous
  8-second budget meant a `plug connect` could kill the cold start Plug.app was
  waiting on, and a second client would kill the next one.
- Operator requests and `plug connect` session setup now time out instead of
  waiting forever. A daemon whose engine is stuck still accepts connections, so
  every status command and every connecting client used to hang with nothing
  reported.
- A crashed daemon is restarted by launchd. Both plists now carry
  `KeepAlive`/`SuccessfulExit=false`, so a panic or a kill self-heals while idle
  grace-period shutdown and `plug stop` still stay stopped.
- A fatal daemon startup error is written to the daemon log. It previously
  reached only stderr, which the app-owned LaunchAgent redirects nowhere.
- Sign-in and sign-out in Plug.app no longer deadlock on a talkative CLI. They
  ran `plug auth` through their own copy of the process-running code, which
  waited for the child to exit before reading its output; a child that filled
  the 64 KB pipe buffer blocked on the write while the app blocked on the wait.
  Both now go through `ProcessRunner`, the app's one process implementation,
  which drains both pipes concurrently and tears down the whole process group
  when a command runs long.
- A daemon that cannot read its own launchd registration now blocks instead of
  reporting repairable drift. `unknown` ownership is an absence of evidence, and
  drift is retried on every trigger, so the app kept walking the adoption path
  against a daemon nobody had proved was its own.
- Booting a legacy launchd job out now re-reads the job's program path first.
  `launchctl bootout` addresses a job by label, and a label reused by a
  different program between inspection and teardown would have been booted out
  on the strength of the earlier job's evidence.
- Pausing downstream connectors no longer blocks the main thread. It shelled out
  to `ps` synchronously with no timeout, so a wedged `ps` froze the whole app;
  it now goes through `ProcessRunner` with the rest.
- A status command can no longer defeat a daemon start. Reading the runtime lock
  takes it and lets it go again, and a start that collided with that momentary
  hold failed outright with "another plug daemon is already running". The start
  now outwaits a probe-length hold before drawing that conclusion.
- `plug`, `plug client list`, and `plug servers` now report the daemon as
  starting rather than stopped while a cold start is in flight. The socket is
  bound only once every upstream is up, so for tens of seconds a healthy daemon
  looked absent, which invited a repair that fought the start.
- One unreachable server can no longer hold up every other server's startup.
  The HTTP and legacy SSE upstream clients were built without a connect
  timeout, so a host that never answers ran until the per-server start timeout
  expired; both now use a ten-second connect bound. OAuth metadata discovery on
  the start path is bounded too, at a sixth of that server's own start budget,
  because the client rmcp builds for it carries a thirty-second timeout and no
  connect bound — exactly the default start timeout, so one unreachable OAuth
  host could consume a server's entire budget. A recorded cold start took
  32.65 s across thirteen servers, of which one server spent 30.18 s inside
  discovery.

### Changed

- Debug and test builds now emit line tables instead of full DWARF, and
  dependencies emit no debug info at all. Panic backtraces still resolve to
  file and line. A cold `cargo build --workspace --all-targets` drops from 40
  to 31 seconds and from 5.05 GB to 3.34 GB.
- Plug.app asks the daemon for the tool list only when it would answer
  differently. The daemon now reports a tool catalog revision on the cheap
  status snapshot; the app used to refetch the whole catalog on a fifteen-second
  timer because it assembled its own fingerprint from server fields and so could
  not see a tool disabled from the CLI. The app also reuses the handshake it
  already negotiated on an open connection instead of renegotiating on every
  poll.
- The operator status snapshot no longer carries upstream branding icons. An
  icon is a base64 data URI, and two servers advertising large ones were about
  half of a snapshot that is polled every couple of seconds. Tool listings still
  carry icons, which is where a client renders them, and `plug servers --output
  json` is unchanged. Together with the catalog change, thirty seconds of app
  polling drops from roughly 1918 KiB across 45 round trips to roughly 262 KiB
  across 30.

## [0.8.1] - 2026-08-26

### Fixed

- Let an app-managed daemon finish one bounded cold start instead of force-
  restarting it every 250 milliseconds while its upstream servers initialize.

## [0.8.0] - 2026-08-26

### Added

- Rebuilt Plug.app around a useful menu-bar popover, with one plain-language
  status, direct repair actions, live server state, connected-app visibility,
  Settings, and Quit in immediate reach.
- Added full Servers, Tools, Connections, and Activity workspaces, including
  searchable tool names, per-tool switches, server editing, app linking,
  server import, remote-client revocation, sign-in and sign-out, and readable
  call attribution.
- Added native macOS 26 Liquid Glass for compact controls and transient
  surfaces, with an accessible material fallback on macOS 14 and 15.

### Changed

- Replaced protocol and service jargon with calm, human language and made
  state readable by symbol and text rather than color alone.
- Refreshes stay responsive while the app is visible, slow down in the
  background, fetch activity incrementally, and avoid reloading the complete
  tool catalog every two seconds.
- Operator IPC v6 lets the signed app load a complete server definition before
  editing, so compact edits preserve advanced settings and credentials.

### Fixed

- Editing a server no longer replaces fields the form did not display with
  empty defaults.
- Activity history now returns the newest bounded calls instead of the oldest
  calls in the retained ring.
- PlugApp's architecture check now runs as a shell CI gate instead of reading
  source files from the signed XCTest host, removing a macOS privacy hang from
  the local test loop.
- Recognize the exact bundle-relative ServiceManagement daemon as app-owned
  when its recorded build is older, then unregister the old app service before
  registering the replacement. Updates no longer strand a valid installation
  behind stale background-service evidence.

## [0.7.5] - 2026-08-25

### Fixed

- Make Plug.app the sole macOS daemon starter whenever a verified app is
  installed. `plug connect` now opens Plug.app for recovery instead of
  recreating the legacy command-line LaunchAgent.
- Automatically reclaim missing or legacy daemon ownership after the user has
  already enabled Plug.app's ServiceManagement agent. First-run adoption still
  requires the original explicit consent.
- Reject production `plug serve --daemon` processes launched outside the
  app-owned launchd job, preventing external supervisors from becoming a
  competing runtime owner. `PLUG_DEV=1` and app-free installs retain their
  development and Linux behavior.

## [0.7.4] - 2026-08-25

### Fixed

- Ignore unrelated launchd jobs that disappear between broad discovery and
  inspection, while preserving fail-closed inspection for Plug's exact daemon
  label.
- Exclude Plug.app's own RunningBoard application job from daemon ownership
  evidence, so the open app cannot block adoption of its background service.
- Preserve the verified `~/.local/bin/plug` shell-link location as legacy
  launchd evidence, allowing the app to adopt the old CLI-managed daemon.
- Stop counting the canonical `~/.local/bin/plug` link as a competing
  install once it points at the bundled executable, so a fully repaired
  installation no longer shows a false "did not converge" warning.
- Name the exact final check that disagreed in the installation drift banner
  instead of listing every possible cause.

## [0.7.3] - 2026-08-25

### Fixed

- Ignore unrelated launchd jobs whose labels merely contain `plug`; only proven
  Plug ownership participates in daemon adoption. The exact
  `com.plug.daemon` label and jobs with Plug executable or ServiceManagement
  evidence remain fail-closed.

## [0.7.2] - 2026-08-25

### Fixed

- Fixed first-run reconciliation when the daemon is stopped. The app now
  accepts Doctor's valid machine-readable failure report and continues into
  daemon adoption instead of stopping before startup.

## [0.7.1] - 2026-08-25

### Fixed

- Fixed an installed-app delegation loop that could make the embedded CLI time
  out while verifying its own version. Plug.app now performs that internal
  version probe without re-entering app discovery.

## [0.7.0] - 2026-08-25

Detailed notes: [Plug 0.7.0](docs/archive/release-notes/RELEASE-NOTES-0.7.0.md).

### Added

- A bounded macOS installation coordinator that reconciles the signed app,
  embedded daemon, command link, MCP client entries, launchd ownership, and
  runtime version as one installation.
- Recognition and conservative migration of supported legacy Plug binaries,
  Homebrew Formula installs, LaunchAgents, and client paths.

### Changed

- Plug.app is now the sole supported public macOS owner of the GUI, `plug`
  command, background daemon, client links, and Sparkle updates. The Homebrew
  Cask installs the app without a competing command binary.
- macOS client linking, command delegation, and repairs resolve the verified
  app executable. Source development is isolated to `plug-dev` with
  `PLUG_DEV=1`; Linux keeps standalone Formula, shell-installer, and archive
  paths.
- Release packaging stages the DMG, signed appcast, app-only Cask, Linux
  artifacts, and checksums through one publication transaction governed by a
  single workspace version.

### Fixed

- App-owned daemon updates now use bounded ownership checks, exact-version IPC
  handshakes, and safe replacement/reconnect behavior, including session replay
  for compatible adapters.
- Recognized Plug state can be repaired without overwriting unrelated files,
  launchd jobs, client entries, configuration, or credentials.

### Removed

- macOS standalone CLI release artifacts and the executable MCPB bundle, which
  could create a second runtime owner.

### Documentation

- Clarified supported installation paths: one Plug.app on macOS from the
  website/GitHub DMG or Homebrew Cask (open it once), Linux Formula/shell/archive
  installs, and isolated source development through `PLUG_DEV=1 plug-dev`. Fresh source
  setup now runs `./scripts/setup-codesigning.sh` before
  `./scripts/dev-reinstall.sh`; development invocations use
  `PLUG_DEV=1 plug-dev`. Plug.app owns the macOS GUI, command line, daemon,
  client links, and updates; headless macOS is unsupported.

## [0.5.2] - 2026-08-25

Detailed notes: [Plug 0.5.2](docs/archive/release-notes/RELEASE-NOTES-0.5.2.md).

### Fixed

- Corrected the embedded LaunchAgent's executable argument so macOS can start the app-owned daemon.
- Made daemon adoption pause legacy connectors during the one-time handoff, wait for the previous process to exit, and require a real IPC-ready daemon before reporting success.
- Made both the app and CLI recognize the real `SMAppService` launchd record, preventing the CLI from replacing app ownership when the daemon is temporarily unavailable.

## [0.5.1] - 2026-08-25

Detailed notes: [Plug 0.5.1](docs/archive/release-notes/RELEASE-NOTES-0.5.1.md).

### Fixed

- Prevented the native app from crashing when macOS completes notification authorization on a background queue.
- Made first-run daemon adoption recognize and replace stale or legacy LaunchAgents, stop an unmanaged older daemon gracefully, and restart the app-owned daemon deterministically.
- Prevented test runtimes from ever registering their temporary binaries as the real macOS background service.

## [0.5.0] - 2026-08-25

Detailed notes: [Plug 0.5.0](docs/archive/release-notes/RELEASE-NOTES-0.5.0.md).

### Added

- A native macOS 14+ menu-bar app with calm health status, server controls,
  connected-client visibility, a redacted activity feed, upstream OAuth repair,
  and settings. The app is a full client of the same daemon used by the CLI.
- A versioned, redacted operator IPC surface for the app: compatibility
  handshake, server/client snapshots, bounded activity history, server
  mutations, and downstream-client revocation.
- LaunchAgent ownership through `SMAppService`, including first-run adoption of
  older CLI-managed installations and a clear restart action after app updates.
- Signed, notarized, and stapled universal `.dmg` distribution, Sparkle 2
  updates with a signed stable appcast, and a Homebrew cask using the identical
  disk image.
- Native notifications for upstream reauthorization and newly authorized
  downstream clients, coalesced so retries and flapping servers cannot spam the
  user.
- A complete official prerelease MCP `2026-07-28` server conformance fixture and
  durable evidence for 22 passing checks with zero failures.

### Changed

- Daemon startup is now single-owner and launchd-managed. The CLI, app, and
  reconnecting clients share one arbitration path instead of competing to spawn
  child daemons.
- Live server edits now flow through daemon verbs and atomic config persistence,
  so the app and CLI cannot create two sources of truth.
- `enable_prefix = false` now does what the configuration promises. Unique tools,
  resources, templates, and prompts pass through unchanged; collisions alone
  fall back to server-qualified names.

### Fixed

- Modern request-scoped progress survives metadata translation, maps RMCP's
  upstream token back to the client's token, and streams on the finite HTTP POST
  response before the final result.
- Concrete URIs expanded from advertised resource templates now route to the
  correct upstream server, with ambiguous cross-server matches rejected.
- The installed app and daemon negotiate an IPC compatibility range and offer a
  useful update/restart action instead of failing opaquely on version skew.

### Security

- The operator activity feed is bounded and redacted at capture time; tool
  arguments, results, credentials, and raw prompts never enter app telemetry.
- Sparkle's EdDSA update signature and Apple's Developer ID signature provide
  independent verification of app updates. Missing signing material fails the
  release before any unsigned artifact can be published.

## [0.4.0] - 2026-08-24

Detailed notes: [MCP 2026 dual-era modernization](docs/archive/release-notes/RELEASE-NOTES-2026-08-04-MCP-2026-DUAL-ERA-MODERNIZATION-codex-5.6-sol.md), [multi-client OAuth](docs/archive/release-notes/RELEASE-NOTES-2026-07-17-MULTI-CLIENT-OAUTH-codex-5.6-sol.md), [RMCP 2.2 upgrade](docs/archive/release-notes/RELEASE-NOTES-2026-07-13-RMCP-2.2-codex-5.6-sol.md), and [July 2026 reliability update](docs/archive/release-notes/RELEASE-NOTES-2026-07-12-codex-5.6-sol.md).

### Added

- Standards-based downstream OAuth for multiple public MCP clients, including RFC 7591 Dynamic Client Registration, OAuth Client ID Metadata Documents, explicit consent, PKCE S256, resource-bound tokens, and client list/revoke commands.
- End-to-end config watcher coverage for normal saves, atomic-renames, parse failures, and unrelated file changes.
- IPC proxy characterization coverage for reconnects, retries, malformed frames, notification ordering, and replayed session state.
- CI checks for the declared Rust 1.88 minimum version, RustSec advisories, and todo-file status consistency.
- Opt-in MCP `2026-07-28` downstream and upstream protocol adapters, with independent global gates and a per-server `legacy`, `auto`, or `modern` negotiation policy.
- Modern task lifecycle support with principal-scoped ownership, retrieval, cancellation, expiry, and disconnect-safe execution.
- Secure native modern-to-modern multi-round tool continuations using integrity-protected, principal-bound, expiring, single-use request state.
- A bounded extension envelope that preserves admitted protocol metadata and W3C trace context without allowing unknown fields to influence authorization, identity, routing, credentials, or continuation state.

### Changed

- Remote MCP clients now connect with only Plug's `/mcp` URL and receive isolated registrations and grants. The old singular client ID, shared secret, and redirect allowlist configuration are intentionally removed; existing remote clients authorize once after upgrading.
- Reconnecting daemon clients now restore capabilities, resource subscriptions, client log level, and other session state before resuming work.
- Catalog refresh fetches resources, templates, and prompts concurrently and avoids repeated server lookups and unnecessary filtered views.
- Oversized artifact writes run on the blocking pool instead of occupying an async runtime worker.
- Native task creation and task teardown now use bounded waits derived from each upstream server's call timeout.
- Split the daemon implementation into focused framing, path, registry, auth, notification, and MCP dispatch modules without changing its public behavior.
- Source builds now require Rust 1.88.
- Upgraded the Rust MCP SDK from RMCP 1.7.0 through 2.2.0 to exactly RMCP 3.1.0. The new protocol path remains default-off while legacy behavior stays available for current clients and servers.
- Migrated to RMCP's spec-aligned content, resource, prompt, task, elicitation, and cancellation APIs.
- Refreshed every direct Rust dependency to its latest compatible stable release, including Keyring 4.1.4, Rand 0.10.2, TOML 1.1.2, and Tower HTTP 0.7.0.

### Fixed

- Full default OAuth grants now admit ordinary tool calls to modern upstreams by including the continuation permission Plug must reserve before the first round can cause side effects.
- Resource subscriptions now serialize upstream transitions per URI, preserve the correct recorded owner, and heal route changes without false success or zombie registry entries.
- HTTP and IPC session teardown now aborts local task execution and forwards bounded cancellation to task-capable upstreams.
- Task creation can no longer recreate records after the owning session has been removed, leak owner guards behind a full request queue, or lose cancellation in the send-to-record window.
- Reloads and reconnects now commit through the same coordination lock, so stale reconnect attempts cannot overwrite newer configuration.
- SSE replay preserves the unsent tail after a delivery failure and no longer clears a sender installed by a racing reconnect.
- Daemon IPC read silence now forces a reconnect instead of holding the session mutex indefinitely.
- Replacement grace tasks now participate in shutdown and a shutdown signal remains latched even when no receiver is present.
- Fixed expired-session counter underflow, pending cancellation replay, a daemon reverse-request busy loop, and closed-channel restoration after deregistration.
- Cancellation notifications without `requestId` are accepted and ignored safely instead of being mapped onto an unrelated active call.
- Downstream stdio and daemon-IPC initialization reject RMCP's announced-but-unimplemented MCP `2026-07-28` revision instead of accidentally negotiating it.
- Pinned `sse-stream` 0.2.4 to match the API required by RMCP 2.2.0 and keep fresh locked builds reproducible.
- Preserved complete TOML document parsing after the TOML 1.x upgrade for client discovery, imports, and doctor checks.
- Made Plug's documented 4 MiB HTTP request limit authoritative instead of Axum's hidden 2 MiB default.
- Local macOS reinstalls now sign and verify a staged binary before atomically replacing the live executable, eliminating the unsigned execution window that could retrigger Keychain prompts.
- Daemon auth-status queries no longer fall back to a missing token mirror's Keychain entry, preventing a read-only diagnostic from freezing IPC and HTTP behind a macOS authorization dialog.
- Engine concurrency tests now launch the prebuilt mock server directly, avoiding parallel `cargo run` lock contention that could exhaust their startup timeout on macOS CI.
- Unbound legacy OAuth credentials now require explicit reauthorization instead of being silently rebound to a newly discovered issuer.
- Startup rejects an unbound legacy OAuth file before probing Keychain, avoiding authorization prompts for credentials that cannot be admitted.
- Modern duplicate in-flight JSON-RPC IDs are rejected atomically, with cancellation and cleanup tied to the exact admitted call.
- Expired durable tasks abort local work and forward bounded upstream cancellation before releasing their quota.
- Authorization-required upstreams now produce a distinct machine-readable protocol outcome rather than a generic unavailable-server error.
- Modern Host validation accepts the configured public URL without weakening unrelated-origin checks, and protocol mismatch responses consistently identify the selected MCP revision.
- Failed stdio discovery probes no longer latch the modern era before a successful discovery response.
- MCP conformance selectors now fail if they match zero tests.

### Security

- Downstream authorization codes, access tokens, refresh tokens, redirects, revocation, quotas, and expiry are isolated by client; registration never grants tool access, and all durable OAuth state is written atomically with owner-only permissions.
- OAuth secret directories are created with owner-only permissions.
- Downstream OAuth state persistence fails closed on unsafe temporary-file permissions and enforces owner-only permissions after rename.
- Expired OAuth records are swept, equivalent scope sets reuse tokens, and client-credentials requests reuse live tokens instead of growing the store on every call.
- Replaced the unmaintained `fs2` lock dependency with `fs4` and removed the duplicate default HTTP stack from `oauth2`.
- Modern continuation state is authenticated and bound to the initiating principal, request, and route, with expiration, replay prevention, bounded storage, and revocation cleanup.

### Known limitations

- `modern_upstream_enabled` and `http.modern_downstream_enabled` default to `false` until real-peer conformance evidence supports changing the defaults.
- Modern downstream negotiation is gated independently across HTTP, stdio, and daemon IPC; existing clients still negotiate the legacy lifecycle by default.
- `subscriptions/listen` is not advertised yet, even though ownership and quota foundations exist.
- Legacy-downstream calls into modern upstream tools, modern-downstream calls into legacy upstream multi-round tools, and task-plus-modern-upstream calls are suppressed rather than risking a stranded request.
- MCP Apps/UI capabilities and synthesized list-result cache directives are not advertised. Admitted metadata can still travel as opaque, policy-limited data.

## [0.3.0] - 2026-05-17

### Added

- SSE reconnect replay for downstream Streamable HTTP sessions.
- Daemon IPC resource subscribe/unsubscribe and targeted resource update delivery.
- Operator source/trust metadata and clearer upstream-vs-inferred tool risk annotations.
- Trace correlation across downstream requests, router calls, retries, reconnects, and upstream HTTP proxying.
- SEP-2243 `Mcp-Method` / `Mcp-Name` validation and upstream header emission.
- Current server-card discovery at `/.well-known/mcp-server-card` with the legacy `/.well-known/mcp.json` alias preserved.
- RFC 9728 protected-resource metadata and client-credentials downstream OAuth support.
- Optional macOS stdio upstream sandboxing.
- Public crates.io packages under `plug-core` and `plug-mcp`.
- Build artifact cleanup helpers for local release and reinstall workflows.

### Changed

- Upgraded `rmcp` to `1.7.0`.
- Replaced the deprecated `serde_yml` parser with `serde_norway`.
- Updated public distribution metadata to the `cyberpapiii/plug` repository and `cyberpapiii/homebrew-tap`.
- Made `cargo install plug-mcp --locked` the primary public Cargo install path.

### Fixed

- Removed obsolete protocol-version response rewrite internals while preserving remote-client compatibility.
- Hardened OAuth discovery/challenge behavior and refresh-token handling.
- Kept daemon, HTTP, and stdio capability surfaces aligned after the hardening pass.

## [0.1.0] - 2026-03-04

### Features

- **core**: MCP multiplexer — shared upstream sessions, 4-tier tool routing
- **transport**: stdio transport for Claude Code, Cursor, Codex, Gemini CLI, and all MCP clients
- **transport**: streamable-HTTP + SSE transport with session management
- **transport**: DNS-rebinding prevention via Origin header validation
- **routing**: prefix-based tool routing (`servername__toolname` convention)
- **routing**: client-aware tool filtering (Cursor ≤40, Windsurf ≤100, VS Code ≤128)
- **routing**: fan-out tool calls with merge and conflict resolution
- **resilience**: circuit breaker per upstream server with half-open recovery
- **resilience**: exponential backoff with jitter on transient failures
- **resilience**: health checks with configurable intervals
- **config**: TOML configuration with layered overrides (file → env → CLI)
- **daemon**: headless daemon mode with PID file and lock management
- **http**: `GET /.well-known/mcp.json` server discovery card endpoint
- **cli**: `plug connect`, `plug status` commands (TUI surface later removed; CLI-first)
- **dist**: single binary, zero runtime dependencies

[Unreleased]: https://github.com/cyberpapiii/plug/compare/v0.8.13...HEAD
[0.7.2]: https://github.com/cyberpapiii/plug/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/cyberpapiii/plug/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/cyberpapiii/plug/compare/v0.6.4...v0.7.0
[0.5.2]: https://github.com/cyberpapiii/plug/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/cyberpapiii/plug/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/cyberpapiii/plug/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/cyberpapiii/plug/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/cyberpapiii/plug/releases/tag/v0.3.0
[0.1.0]: https://github.com/cyberpapiii/plug/releases/tag/v0.1.0
