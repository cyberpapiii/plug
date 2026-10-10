// Measures the installed Plug.app from the outside. Run through
// scripts/perf-app.sh, which compiles this file and passes the daemon's pid.
//
// It reads process accounting for CPU, wakeups and memory, the window list for
// launch times, and drives the window through accessibility. While an action
// runs it asks the app one cheap accessibility question over and over; the app
// answers on its main thread, so a slow answer is a main-thread stall of that
// length, and a stall longer than one frame is counted as dropped frames.
// Having an accessibility client attached makes every action a little slower
// than it is for a person, equally before and after a change.

import AppKit
import ApplicationServices
import Darwin

let appPath = "/Applications/Plug.app"
let executablePath = appPath + "/Contents/MacOS/Plug"
let frame = 1000.0 / 60.0
/// Answers slower than this count as the main thread being busy.
let stallFloor = 4.0

func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1_000_000 }
func pause(_ ms: Double) { usleep(UInt32(ms * 1000)) }
func row(_ name: String, _ value: String) { print("| \(name) | \(value) |") }
func ms(_ value: Double) -> String { String(format: "%.0f ms", value) }
func note(_ text: String) { FileHandle.standardError.write(Data((text + "\n").utf8)) }

func median(_ values: [Double]) -> Double {
    guard !values.isEmpty else { return 0 }
    let sorted = values.sorted()
    return sorted[sorted.count / 2]
}

// MARK: Processes

/// Asks the kernel, not `NSWorkspace`: without a run loop its list of running
/// apps never changes, and this tool quits and opens Plug.
func plugPID() -> pid_t? {
    var pids = [pid_t](repeating: 0, count: 4096)
    let count = proc_listallpids(&pids, Int32(pids.count * MemoryLayout<pid_t>.size))
    guard count > 0 else { return nil }
    var path = [CChar](repeating: 0, count: 4096)
    return pids.prefix(Int(count)).first { pid in
        pid > 0 && proc_pidpath(pid, &path, UInt32(path.count)) > 0 && String(cString: path) == executablePath
    }
}

struct Usage {
    let cpuMs: Double
    let wakeups: Double
    let footprintMB: Double
}

func usage(_ pid: pid_t) -> Usage? {
    var info = rusage_info_v4()
    let result = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) {
            proc_pid_rusage(pid, RUSAGE_INFO_V4, $0)
        }
    }
    guard result == 0 else { return nil }
    var timebase = mach_timebase_info_data_t()
    mach_timebase_info(&timebase)
    let ticks = Double(info.ri_user_time + info.ri_system_time)
    return Usage(
        cpuMs: ticks * Double(timebase.numer) / Double(timebase.denom) / 1_000_000,
        wakeups: Double(info.ri_pkg_idle_wkups + info.ri_interrupt_wkups),
        footprintMB: Double(info.ri_phys_footprint) / 1_048_576
    )
}

/// CPU share, wakeups per second and memory of each process over a quiet spell.
func idle(_ label: String, seconds: Double, pids: [(String, pid_t)]) {
    let before = pids.map { usage($0.1) }
    pause(seconds * 1000)
    for (index, (name, pid)) in pids.enumerated() {
        guard let start = before[index], let end = usage(pid) else { continue }
        row("Idle CPU, \(name), \(label)", String(format: "%.2f %%", (end.cpuMs - start.cpuMs) / (seconds * 10)))
        row("Idle wakeups, \(name), \(label)", String(format: "%.1f per second", (end.wakeups - start.wakeups) / seconds))
        row("Memory, \(name), \(label)", String(format: "%.0f MB", end.footprintMB))
    }
}

// MARK: Accessibility

func value(_ element: AXUIElement, _ name: String) -> AnyObject? {
    var result: AnyObject?
    AXUIElementCopyAttributeValue(element, name as CFString, &result)
    return result
}

func children(_ element: AXUIElement) -> [AXUIElement] { value(element, "AXChildren") as? [AXUIElement] ?? [] }
func role(_ element: AXUIElement) -> String { value(element, "AXRole") as? String ?? "" }
func label(_ element: AXUIElement) -> String { value(element, "AXDescription") as? String ?? "" }
func title(_ element: AXUIElement) -> String { value(element, "AXTitle") as? String ?? "" }

func press(_ element: AXUIElement) { AXUIElementPerformAction(element, "AXPress" as CFString) }

/// Every element under `root` that passes `test`, without walking into rows.
func find(_ root: AXUIElement, depth: Int = 9, _ test: (AXUIElement) -> Bool) -> [AXUIElement] {
    var found: [AXUIElement] = []
    func walk(_ element: AXUIElement, _ level: Int) {
        if test(element) { found.append(element) }
        guard level < depth, role(element) != "AXRow" else { return }
        for child in children(element) { walk(child, level + 1) }
    }
    walk(root, 0)
    return found
}

struct Plug {
    let pid: pid_t
    let app: AXUIElement

    init?(_ pid: pid_t?) {
        guard let pid else { return nil }
        self.pid = pid
        app = AXUIElementCreateApplication(pid)
    }

    var windows: [AXUIElement] { value(app, "AXWindows") as? [AXUIElement] ?? [] }
    var hasMenuBarIcon: Bool {
        guard let bar = value(app, "AXExtrasMenuBar") else { return false }
        // swiftlint:disable:next force_cast
        return !children(bar as! AXUIElement).isEmpty
    }
    var main: AXUIElement? { windows.first { value($0, "AXIdentifier") as? String == "main" } }
    var outlines: [AXUIElement] { main.map { find($0) { role($0) == "AXOutline" } } ?? [] }
    var sidebar: AXUIElement? { outlines.first { label($0) == "Sidebar" } }
    var list: AXUIElement? { outlines.first { label($0) != "Sidebar" } }

    func rows(_ outline: AXUIElement?) -> [AXUIElement] {
        outline.map { children($0).filter { role($0) == "AXRow" } } ?? []
    }

    func toolbarButton(_ names: String...) -> AXUIElement? {
        guard let main else { return nil }
        return find(main, depth: 2) { role($0) == "AXButton" && names.contains(label($0)) }.first
    }

    func menuItem(_ menu: String, _ item: String) -> AXUIElement? {
        guard let bar = value(app, "AXMenuBar") else { return nil }
        // swiftlint:disable:next force_cast
        let menus = children(bar as! AXUIElement).filter { title($0) == menu }
        return menus.flatMap { find($0, depth: 2) { title($0) == item } }.first
    }

    func ping() { _ = value(app, "AXFrontmost") }
}

struct Stall {
    var blocked = 0.0
    var longest = 0.0
    var settled = 0.0
    var dropped = 0
}

extension Plug {
    /// Runs `action`, then watches the main thread until `window` ms have
    /// passed: how long it was blocked in total, the longest single block, when
    /// it last came free, and how many frames those blocks swallowed.
    func watch(for window: Double = 900, _ action: () -> Void) -> Stall {
        var stall = Stall()
        let start = now()
        func record(_ length: Double) {
            guard length > stallFloor else { return }
            stall.blocked += length
            stall.longest = max(stall.longest, length)
            stall.dropped += Int(length / frame)
            stall.settled = now() - start
        }
        action()
        record(now() - start)
        while now() - start < window {
            let asked = now()
            ping()
            record(now() - asked)
            pause(1)
        }
        return stall
    }

    func report(_ name: String, _ stalls: [Stall]) {
        guard !stalls.isEmpty else { return row(name, "could not run") }
        let settled = stalls.map(\.settled)
        row(name, "\(ms(median(settled))) typical, \(ms(settled.max() ?? 0)) worst; "
            + "longest freeze \(ms(median(stalls.map(\.longest)))) typical, \(ms(stalls.map(\.longest).max() ?? 0)) worst; "
            + "\(Int(median(stalls.map { Double($0.dropped) }))) dropped frames typical, \(stalls.map(\.dropped).max() ?? 0) worst")
    }

    func select(_ row: AXUIElement) {
        AXUIElementSetAttributeValue(row, "AXSelected" as CFString, kCFBooleanTrue)
    }

    func selected(in outline: AXUIElement?) -> Int? {
        rows(outline).firstIndex { value($0, "AXSelected") as? Bool == true }
    }

    /// Selects each row of an outline in turn, `rounds` times over.
    func cycle(_ outline: () -> AXUIElement?, rounds: Int) -> [Stall] {
        var stalls: [Stall] = []
        for _ in 0..<rounds {
            for index in rows(outline()).indices {
                let rows = rows(outline())
                guard index < rows.count else { break }
                let stall = watch { select(rows[index]) }
                // A heading row refuses the selection; that is not an action.
                if selected(in: outline()) == index { stalls.append(stall) }
            }
        }
        return stalls
    }

    func actions() {
        guard main != nil else { return note("actions: Plug has no window open") }
        let sections = rows(sidebar)
        guard let first = sections.first else { return note("actions: no sidebar") }
        let startedOn = selected(in: sidebar) ?? 0

        report("Switch section", cycle({ sidebar }, rounds: 3))

        select(first)
        pause(700)
        let servers = cycle({ list }, rounds: 3)
        report("Select a server", servers)

        // The slowest server to open is the one with the most to draw.
        var selectable: [Int] = []
        var slowest = 0
        var slowestTime = 0.0
        for index in rows(list).indices {
            let stall = watch(for: 600) { select(rows(list)[index]) }
            guard selected(in: list) == index else { continue }
            selectable.append(index)
            if stall.blocked > slowestTime {
                slowest = index
                slowestTime = stall.blocked
            }
        }
        var largest: [Stall] = []
        if let other = selectable.first(where: { $0 != slowest }) {
            for _ in 0..<5 {
                select(rows(list)[other])
                pause(600)
                largest.append(watch { select(rows(list)[slowest]) })
            }
        }
        report("Select the largest server", largest)

        var sidebarStalls: [Stall] = []
        for _ in 0..<6 {
            guard let button = toolbarButton("Hide Sidebar", "Show Sidebar") else { break }
            sidebarStalls.append(watch { press(button) })
        }
        report("Hide or show the sidebar", sidebarStalls)

        if let main, let size = value(main, "AXSize") {
            var original = CGSize.zero
            // swiftlint:disable:next force_cast
            AXValueGetValue(size as! AXValue, .cgSize, &original)
            var resizes: [Stall] = []
            for step in 0..<24 {
                let grow = CGFloat(step < 12 ? step + 1 : 24 - step - 1) * 24
                var next = CGSize(width: original.width + grow, height: original.height + grow / 2)
                guard let boxed = AXValueCreate(.cgSize, &next) else { break }
                resizes.append(watch(for: 120) { AXUIElementSetAttributeValue(main, "AXSize" as CFString, boxed) })
            }
            let total = resizes.reduce(0) { $0 + $1.blocked }
            row("Resize the window, 24 steps on the largest server",
                "\(ms(total / 24)) blocked per step, longest \(ms(resizes.map(\.longest).max() ?? 0)), "
                    + "\(resizes.reduce(0) { $0 + $1.dropped }) dropped frames in all")
        }

        if let main, let bar = find(main, depth: 8, { role($0) == "AXScrollBar" }).last {
            var scrolls: [Stall] = []
            for step in 0...20 {
                let position = (step <= 10 ? Double(step) : Double(20 - step)) / 10
                scrolls.append(watch(for: 100) {
                    AXUIElementSetAttributeValue(bar, "AXValue" as CFString, position as CFNumber)
                })
            }
            row("Scroll the largest server, 21 steps",
                "\(ms(scrolls.reduce(0) { $0 + $1.blocked } / 21)) blocked per step, longest \(ms(scrolls.map(\.longest).max() ?? 0)), "
                    + "\(scrolls.reduce(0) { $0 + $1.dropped }) dropped frames in all")
        }

        var settings: [Stall] = []
        for _ in 0..<4 {
            guard let item = menuItem("Plug", "Settings…") else { break }
            settings.append(watch { press(item) })
            for window in windows where value(window, "AXIdentifier") as? String != "main" {
                if let close = value(window, "AXCloseButton") {
                    // swiftlint:disable:next force_cast
                    press(close as! AXUIElement)
                }
            }
            pause(500)
        }
        report("Open Settings", settings)

        var panel: [Stall] = []
        if let extras = value(app, "AXExtrasMenuBar"),
           // swiftlint:disable:next force_cast
           let icon = children(extras as! AXUIElement).first {
            for _ in 0..<4 {
                panel.append(watch { press(icon) })
                press(icon)
                pause(500)
            }
        }
        report("Open the menu bar panel", panel)

        if startedOn < rows(sidebar).count { select(rows(sidebar)[startedOn]) }
    }
}

// MARK: Launch

func open() {
    let task = Process()
    task.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    task.arguments = ["-a", appPath]
    try? task.run()
}

/// Polls until `test` holds, and returns how long that took.
func wait(upTo limit: Double, _ test: () -> Bool) -> Double? {
    let start = now()
    while now() - start < limit {
        if test() { return now() - start }
        pause(2)
    }
    return nil
}

func usable(_ plug: Plug) -> Bool { !plug.rows(plug.sidebar).isEmpty }

func launch(runs: Int) {
    var icon: [Double] = []
    var window: [Double] = []
    var ready: [Double] = []
    var reopen: [Double] = []
    var reopenReady: [Double] = []

    for _ in 0..<runs {
        if let pid = plugPID() {
            NSRunningApplication(processIdentifier: pid)?.terminate()
            guard wait(upTo: 30_000, { kill(pid, 0) != 0 }) != nil else { return note("launch: Plug did not quit") }
            pause(1500)
        }
        let start = now()
        open()
        // The system draws the menu bar icon itself, so Plug owns no window
        // for it. Accessibility lists the icon once Plug has put it up.
        var found: Plug?
        guard wait(upTo: 20_000, {
            found = Plug(plugPID())
            return found?.hasMenuBarIcon ?? false
        }) != nil, let plug = found else { return note("launch: the menu bar icon never showed") }
        icon.append(now() - start)
        // Opened by hand, Plug may show its window; opened at login it does not.
        if wait(upTo: 5000, { plug.main != nil }) != nil {
            window.append(now() - start)
            if wait(upTo: 8000, { usable(plug) }) != nil { ready.append(now() - start) }
        }
        pause(6000)

        // Reopening a running Plug whose window is closed.
        if let main = plug.main, let close = value(main, "AXCloseButton") {
            // swiftlint:disable:next force_cast
            press(close as! AXUIElement)
            guard wait(upTo: 5000, { plug.main == nil }) != nil else { continue }
            pause(1500)
        }
        let again = now()
        open()
        if wait(upTo: 8000, { plug.main != nil }) != nil {
            reopen.append(now() - again)
            if wait(upTo: 8000, { usable(plug) }) != nil { reopenReady.append(now() - again) }
        }
        pause(1500)
    }

    func line(_ name: String, _ values: [Double]) {
        row(name, values.isEmpty ? "did not happen" : "\(ms(median(values))) typical, \(ms(values.max() ?? 0)) worst of \(values.count)")
    }
    line("Launch: menu bar icon shows", icon)
    // Plug starts in the menu bar alone unless it is set to show its window.
    if !window.isEmpty {
        line("Launch: window on screen", window)
        line("Launch: window usable", ready)
    }
    line("Open the window: on screen", reopen)
    line("Open the window: usable", reopenReady)
}

// MARK: Long session

/// Uses the window the way a person would for `minutes`, and reports memory
/// along the way.
func soak(minutes: Double, daemon: pid_t?) {
    guard let plug = Plug(plugPID()), plug.main != nil else { return note("soak: Plug has no window open") }
    let start = now()
    var readings: [Double] = [usage(plug.pid)?.footprintMB ?? 0]
    let daemonStart = daemon.flatMap { usage($0)?.footprintMB }
    var turn = 0
    var nextReading = 60_000.0
    while now() - start < minutes * 60_000 {
        let sections = plug.rows(plug.sidebar)
        let servers = plug.rows(plug.list)
        switch turn % 4 {
        case 0 where !sections.isEmpty: plug.select(sections[(turn / 4) % sections.count])
        case 3: if let button = plug.toolbarButton("Hide Sidebar", "Show Sidebar") { press(button) }
        default: if !servers.isEmpty { plug.select(servers[turn % servers.count]) }
        }
        turn += 1
        pause(3000)
        if now() - start > nextReading {
            readings.append(usage(plug.pid)?.footprintMB ?? 0)
            nextReading += 60_000
        }
    }
    if plug.toolbarButton("Show Sidebar") != nil, let button = plug.toolbarButton("Show Sidebar") { press(button) }
    let end = usage(plug.pid)?.footprintMB ?? 0
    row("Memory after \(Int(minutes)) minutes of use, app",
        String(format: "%.0f MB (started at %.0f, peak %.0f)", end, readings[0], max(end, readings.max() ?? 0)))
    if let daemon, let daemonStart, let daemonEnd = usage(daemon)?.footprintMB {
        row("Memory after \(Int(minutes)) minutes of use, daemon", String(format: "%.0f MB (started at %.0f)", daemonEnd, daemonStart))
    }
}

// MARK: Main

guard AXIsProcessTrusted() else {
    note("perf-app: this terminal needs Accessibility permission (System Settings, Privacy & Security)")
    exit(1)
}

var arguments = Array(CommandLine.arguments.dropFirst())
let daemon = arguments.isEmpty ? nil : pid_t(arguments.removeFirst())
let mode = arguments.first ?? "all"

func processes() -> [(String, pid_t)] {
    var list: [(String, pid_t)] = []
    if let pid = plugPID() { list.append(("app", pid)) }
    if let daemon { list.append(("daemon", daemon)) }
    return list
}

/// Closes the main window, runs `body`, and opens it again.
func withWindowClosed(_ body: () -> Void) {
    guard let plug = Plug(plugPID()), let main = plug.main, let close = value(main, "AXCloseButton") else { return body() }
    // swiftlint:disable:next force_cast
    press(close as! AXUIElement)
    pause(3000)
    body()
    open()
    pause(3000)
}

switch mode {
case "launch":
    launch(runs: 3)
case "idle":
    idle("window open", seconds: 60, pids: processes())
    withWindowClosed { idle("window closed", seconds: 60, pids: processes()) }
case "actions":
    Plug(plugPID())?.actions()
case "soak":
    soak(minutes: Double(arguments.dropFirst().first ?? "") ?? 30, daemon: daemon)
default:
    launch(runs: 3)
    // The launch pass and the first reads are over well inside two minutes.
    pause(120_000)
    idle("window open", seconds: 60, pids: processes())
    withWindowClosed { idle("window closed", seconds: 60, pids: processes()) }
    Plug(plugPID())?.actions()
}
