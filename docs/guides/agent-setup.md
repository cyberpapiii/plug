# Set up Plug on this Mac

You are an agent setting up Plug for the person you work with. Plug is an MCP
gateway: one place on this Mac that holds every server and gives its tools to
every client.

- A **server** provides tools. It runs on this Mac or is a remote service.
- A **client** uses tools: Claude, Codex, Cursor, a script.
- Plug sits between them. Add a server once and every connected client has it.

Use the `plug` command for everything. Add `--output json` to any command for
output you can parse. If `plug` is not on the PATH, use
`/Applications/Plug.app/Contents/Resources/plug`.

## Steps

1. **Check Plug is running.** Run `plug status`. If Plug.app is not installed,
   ask the person to install it (`brew install --cask cyberpapiii/tap/plug-app`,
   or the download at https://github.com/cyberpapiii/plug/releases) and open it
   once. Plug.app starts and owns the background service. Do not start, stop,
   or kill that service yourself.
2. **See what exists.** Run `plug servers` and `plug clients`.
3. **Bring in the servers the person already has.** Run
   `plug import --dry-run` and show them the list. When they agree, run
   `plug import --yes`.
4. **Add any other server they ask for.**
   - A local command:
     `plug server add <name> --command <command> --args <a,b,c> --env KEY=VALUE`
   - A remote service: `plug server add <name> --url <url> --auth oauth`
   - A second account on a server that is already there:
     `plug server add-account <server> <account>`
   - Sign in to a server that needs it: `plug auth login --server <name>`.
     This opens a browser. The person finishes the sign-in.
5. **Connect clients.** Run `plug clients` to see which are installed, then
   `plug link --yes <client> ...` for the ones the person wants, or
   `plug link --yes --all`. A client reads its settings at launch, so ask the
   person to restart each client you linked.
6. **Check it works.** Run `plug status`, then `plug doctor`, then `plug tools`.
   Every server should be working and list its tools.
7. **Report.** Say which servers and clients are set up, and list anything
   left for the person: a sign-in to finish, a client to restart.

## Rules

- Change client settings only through `plug link` and `plug unlink`. Do not
  edit a client's configuration file by hand.
- Do not ask the person to paste a token or password into the chat. Sign-ins
  go through `plug auth login`. For a key, have the person run
  `plug secret set <name>`, which asks them for it and keeps it in the
  Keychain.
- Ask before you remove a server or unlink a client.
- When something is wrong, `plug doctor` says what and how to fix it. Logs are
  in `~/Library/Logs/plug/`.
