import AppKit
import PlugIPC


/// Finding the icon a server offers, or the one its web site carries.
enum SiteIcon {
    private static let maxPage = 2_000_000
    private static let maxImage = 1_000_000

    /// The icon to use out of the ones a server offers: the largest.
    static func best(of icons: [ServerIcon]) -> ServerIcon? {
        func side(_ icon: ServerIcon) -> Int {
            (icon.sizes ?? []).map { size -> Int in
                if size.lowercased() == "any" { return 4096 }
                return Int(size.lowercased().split(separator: "x").first ?? "") ?? 0
            }.max() ?? 0
        }
        var best: ServerIcon?
        for icon in icons where best.map({ side(icon) > side($0) }) ?? true { best = icon }
        return best
    }

    /// The bytes inside a base64 `data:` address.
    static func data(fromDataURI source: String) -> Data? {
        guard source.lowercased().hasPrefix("data:"), let comma = source.firstIndex(of: ","),
              source[..<comma].lowercased().hasSuffix(";base64") else { return nil }
        return Data(base64Encoded: String(source[source.index(after: comma)...]))
    }

    /// The hosts to ask for an icon, most likely first: the server's own,
    /// the site above it, then the page the server named. HTTPS only, so a
    /// server on this Mac or on the local network is never asked.
    static func hosts(server: String?, website: String?) -> [String] {
        var found: [String] = []
        for address in [server, website] {
            guard let address, let url = URL(string: address), url.scheme?.lowercased() == "https",
                  let host = url.host?.lowercased(), !host.isEmpty else { continue }
            for candidate in [host, parent(of: host)].compactMap({ $0 }) where !found.contains(candidate) {
                found.append(candidate)
            }
        }
        return found
    }

    /// Makers' web sites, by the word their servers and clients are named
    /// with. A local command has no address of its own to ask, and its
    /// name is all there is to go on.
    private static let brands: [String: String] = [
        "airtable": "airtable.com", "amp": "ampcode.com", "amplitude": "amplitude.com", "anthropic": "anthropic.com",
        "asana": "asana.com", "atlassian": "atlassian.com", "aws": "aws.amazon.com",
        "box": "box.com", "brave": "brave.com", "bun": "bun.sh", "canva": "canva.com",
        "clickup": "clickup.com", "cloudflare": "cloudflare.com", "confluence": "atlassian.com",
        "context7": "context7.com", "datadog": "datadoghq.com", "discord": "discord.com",
        "docker": "docker.com", "dropbox": "dropbox.com", "elevenlabs": "elevenlabs.io",
        "exa": "exa.ai", "figma": "figma.com", "firecrawl": "firecrawl.dev",
        "gemini": "gemini.google.com", "github": "github.com", "gitlab": "gitlab.com",
        "grafana": "grafana.com", "homeassistant": "home-assistant.io", "hubspot": "hubspot.com",
        "huggingface": "huggingface.co", "intercom": "intercom.com", "jira": "atlassian.com",
        "kimi": "kimi.com", "lmstudio": "lmstudio.ai", "muse": "meta.ai", "openclaw": "openclaw.ai", "krisp": "krisp.ai", "kubernetes": "kubernetes.io",
        "linear": "linear.app", "miro": "miro.com", "mistral": "mistral.ai",
        "mixpanel": "mixpanel.com", "monday": "monday.com", "mongodb": "mongodb.com",
        "mysql": "mysql.com", "netlify": "netlify.com", "node": "nodejs.org",
        "notion": "notion.com", "obsidian": "obsidian.md", "openai": "openai.com",
        "oura": "ouraring.com", "pagerduty": "pagerduty.com", "paypal": "paypal.com",
        "perplexity": "perplexity.ai", "plaid": "plaid.com", "playwright": "playwright.dev",
        "postgres": "postgresql.org", "postgresql": "postgresql.org", "posthog": "posthog.com",
        "python": "python.org", "python3": "python.org", "qwen": "qwen.ai",
        "raycast": "raycast.com", "reddit": "reddit.com", "redis": "redis.io",
        "replicate": "replicate.com", "salesforce": "salesforce.com", "sentry": "sentry.io",
        "shopify": "shopify.com", "slack": "slack.com", "snowflake": "snowflake.com",
        "spotify": "spotify.com", "sqlite": "sqlite.org", "strava": "strava.com",
        "stripe": "stripe.com", "supabase": "supabase.com", "svelte": "svelte.dev",
        "tavily": "tavily.com", "telegram": "telegram.org", "todoist": "todoist.com",
        "trello": "trello.com", "twilio": "twilio.com", "vercel": "vercel.com",
        "xero": "xero.com", "youtube": "youtube.com", "zapier": "zapier.com",
        "zendesk": "zendesk.com", "zoom": "zoom.us",
    ]

    /// The maker's site for a name: the whole name, then each word of it.
    /// "oura" and "oura-mcp" both give Oura's.
    static func brandHost(forName name: String) -> String? {
        if let host = brands[AppIcons.lookupKey(name)] { return host }
        let words = name.lowercased().split(whereSeparator: { !$0.isLetter && !$0.isNumber })
        return words.lazy.compactMap { brands[String($0)] }.first
    }

    /// A package a registry can describe.
    struct Package: Equatable, Sendable {
        enum Registry: Sendable { case npm, pypi }
        let registry: Registry
        let name: String
    }

    /// The package a command runs, when it runs one straight from a
    /// registry: `npx -y @scope/thing@1.2` runs `@scope/thing`.
    static func package(command: String?, args: [String]) -> Package? {
        guard let tool = command?.split(separator: "/").last else { return nil }
        let registry: Package.Registry
        switch tool {
        case "npx", "bunx", "pnpx": registry = .npm
        case "uvx", "pipx": registry = .pypi
        default: return nil
        }
        guard var name = args.first(where: { !$0.hasPrefix("-") && $0 != "run" }) else { return nil }
        // Drop a version, keeping the "@" a scope starts with.
        if let at = name.lastIndex(of: "@"), at != name.startIndex { name = String(name[..<at]) }
        if registry == .pypi, let cut = name.firstIndex(where: { "=<>[".contains($0) }) { name = String(name[..<cut]) }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "@/._-"))
        guard !name.isEmpty, name.unicodeScalars.allSatisfy(allowed.contains) else { return nil }
        return Package(registry: registry, name: name)
    }

    /// Where to look for a package's icon, given the links its registry
    /// lists: its own site, and its owner's picture on GitHub.
    static func places(forPackageLinks links: [String]) -> (hosts: [String], picture: URL?) {
        var hosts: [String] = []
        var picture: URL?
        for link in links {
            let address = link.replacingOccurrences(of: "git+", with: "")
            guard let url = URL(string: address), let host = url.host?.lowercased() else { continue }
            if host == "github.com" || host == "www.github.com" {
                let owner = url.path.split(separator: "/").first.map(String.init) ?? ""
                if picture == nil, !owner.isEmpty { picture = URL(string: "https://github.com/\(owner).png") }
            } else if url.scheme == "https", !host.hasSuffix("npmjs.com"), !host.hasSuffix("pypi.org"),
                      !hosts.contains(host) {
                hosts.append(host)
            }
        }
        return (hosts, picture)
    }

    /// The icon of a package's site, or of its owner, as PNG data.
    static func find(package: Package) async -> Data? {
        let address = switch package.registry {
        case .npm: "https://registry.npmjs.org/\(package.name)/latest"
        case .pypi: "https://pypi.org/pypi/\(package.name)/json"
        }
        let session = makeSession()
        defer { session.invalidateAndCancel() }
        guard let url = URL(string: address), let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200, data.count <= maxPage,
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        var links: [String] = []
        let facts = (object["info"] as? [String: Any]) ?? object
        for key in ["homepage", "home_page"] {
            if let link = facts[key] as? String { links.append(link) }
        }
        if let repository = facts["repository"] as? [String: Any], let link = repository["url"] as? String {
            links.append(link)
        } else if let link = facts["repository"] as? String {
            links.append(link)
        }
        if let urls = facts["project_urls"] as? [String: Any] {
            links += urls.keys.sorted().compactMap { urls[$0] as? String }
        }
        let places = places(forPackageLinks: links)
        if let found = await find(hosts: places.hosts) { return found }
        guard let picture = places.picture else { return nil }
        return await image(at: picture, session: session)
    }

    /// The site one label up: `mcp.example.com` gives `example.com`. Nil
    /// for an address made of numbers and for a name with nothing above it.
    static func parent(of host: String) -> String? {
        let labels = host.split(separator: ".")
        guard labels.count >= 3, !host.contains(":"),
              labels.contains(where: { $0.contains(where: \.isLetter) }) else { return nil }
        return labels.dropFirst().joined(separator: ".")
    }

    /// The icons a page links to, best first, then the two addresses sites
    /// keep an icon at by habit. HTTPS only.
    static func candidates(inHTML html: String, base: URL) -> [URL] {
        var scored: [(score: Int, url: URL)] = []
        let range = NSRange(html.startIndex..., in: html)
        let tags = (try? NSRegularExpression(pattern: "<link\\b[^>]*>", options: .caseInsensitive))?
            .matches(in: html, range: range) ?? []
        for match in tags {
            guard let tagRange = Range(match.range, in: html) else { continue }
            let tag = String(html[tagRange])
            guard let rel = attribute("rel", in: tag)?.lowercased(), rel.contains("icon"), !rel.contains("mask"),
                  let href = attribute("href", in: tag),
                  let url = URL(string: href, relativeTo: base)?.absoluteURL else { continue }
            let side = attribute("sizes", in: tag).flatMap { Int($0.lowercased().split(separator: "x").first ?? "") } ?? 0
            scored.append(((rel.contains("apple-touch") ? 1000 : 0) + side, url))
        }
        var found = scored.enumerated()
            .sorted { $0.element.score != $1.element.score ? $0.element.score > $1.element.score : $0.offset < $1.offset }
            .map(\.element.url)
        for path in ["/apple-touch-icon.png", "/favicon.ico"] {
            if let url = URL(string: path, relativeTo: base)?.absoluteURL { found.append(url) }
        }
        var seen = Set<URL>()
        return Array(found.filter { $0.scheme == "https" && seen.insert($0).inserted }.prefix(6))
    }

    private static func attribute(_ name: String, in tag: String) -> String? {
        let pattern = "\\b\(name)\\s*=\\s*(?:\"([^\"]*)\"|'([^']*)'|([^\\s>]+))"
        guard let expression = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive),
              let match = expression.firstMatch(in: tag, range: NSRange(tag.startIndex..., in: tag)) else { return nil }
        for group in 1...3 {
            if let range = Range(match.range(at: group), in: tag) { return String(tag[range]) }
        }
        return nil
    }

    /// The first icon any of `hosts` gives up, as PNG data.
    static func find(hosts: [String]) async -> Data? {
        let session = makeSession()
        defer { session.invalidateAndCancel() }
        for host in hosts {
            guard let root = URL(string: "https://\(host)/") else { continue }
            var html = ""
            var base = root
            if let (data, response) = try? await session.data(from: root), data.count <= maxPage,
               (response as? HTTPURLResponse)?.statusCode == 200 {
                html = String(decoding: data, as: UTF8.self)
                base = response.url ?? root
            }
            for url in candidates(inHTML: html, base: base) {
                if let data = await image(at: url, session: session) { return data }
            }
        }
        return nil
    }

    /// The picture at `url`, as PNG data.
    static func image(at url: URL) async -> Data? {
        let session = makeSession()
        defer { session.invalidateAndCancel() }
        return await image(at: url, session: session)
    }

    private static func image(at url: URL, session: URLSession) async -> Data? {
        guard url.scheme == "https", let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200, data.count <= maxImage else { return nil }
        return IconTile.png(from: data)
    }

    /// No cookies, no cache, no credentials: the request says nothing
    /// about this Mac beyond its address.
    private static func makeSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 8
        configuration.timeoutIntervalForResource = 15
        configuration.httpShouldSetCookies = false
        configuration.httpCookieAcceptPolicy = .never
        configuration.urlCache = nil
        configuration.httpAdditionalHeaders = ["User-Agent": "Mozilla/5.0 (Macintosh) Plug"]
        return URLSession(configuration: configuration)
    }
}

/// Any picture, made to sit beside app icons: the same size, the same
/// rounded shape, and a tile behind a bare mark so it shows on any
/// background.
enum IconTile {
    private static let side = 192

    /// Nil when the data is not a picture or is too small to show.
    static func png(from data: Data) -> Data? {
        guard let image = NSImage(data: data) else { return nil }
        var proposed = CGRect(x: 0, y: 0, width: side, height: side)
        guard let whole = image.cgImage(forProposedRect: &proposed, context: nil, hints: nil),
              whole.width >= 32, whole.height >= 32 else { return nil }
        // A picture often carries an empty margin of its own. Measure and
        // draw what is inside it, or the margin is added twice.
        guard let box = shape(of: whole)?.box,
              let source = whole.cropping(to: box),
              let shape = shape(of: source) else { return nil }

        guard let context = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8,
            bytesPerRow: side * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        context.interpolationQuality = .high
        let canvas = CGRect(x: 0, y: 0, width: side, height: side)
        // App icons keep a margin inside their square; so does this.
        let tile = canvas.insetBy(dx: canvas.width * 0.08, dy: canvas.width * 0.08)
        if shape.cornersClear, shape.clearShare < 0.3 {
            // Already shaped like an icon. Leave its shape alone.
            context.draw(source, in: fit(source, in: tile))
        } else {
            let radius = tile.width * 0.24
            context.addPath(CGPath(roundedRect: tile, cornerWidth: radius, cornerHeight: radius, transform: nil))
            context.clip()
            let bare = shape.clearShare >= 0.3 || source.width != source.height
            let shade: CGFloat = bare && shape.isLight ? 0.11 : 1
            context.setFillColor(CGColor(red: shade, green: shade, blue: shade, alpha: 1))
            context.fill(tile)
            let margin = bare ? tile.width * 0.16 : 0
            context.draw(source, in: fit(source, in: tile.insetBy(dx: margin, dy: margin)))
        }
        guard let result = context.makeImage() else { return nil }
        return NSBitmapImageRep(cgImage: result).representation(using: .png, properties: [:])
    }

    /// The largest rectangle of the picture's proportions inside `rect`.
    private static func fit(_ image: CGImage, in rect: CGRect) -> CGRect {
        let scale = min(rect.width / CGFloat(image.width), rect.height / CGFloat(image.height))
        let width = CGFloat(image.width) * scale, height = CGFloat(image.height) * scale
        return CGRect(x: rect.midX - width / 2, y: rect.midY - height / 2, width: width, height: height)
    }

    private struct Shape {
        /// The part of the picture that is not empty, in its own pixels.
        let box: CGRect
        /// How much of the picture is empty.
        let clearShare: Double
        let isLight: Bool
        let cornersClear: Bool
    }

    /// What a small copy of the picture says about it. Nil for a picture
    /// with nothing in it.
    private static func shape(of image: CGImage) -> Shape? {
        let probe = 64
        guard let measure = CGContext(
            data: nil, width: probe, height: probe, bitsPerComponent: 8,
            bytesPerRow: probe * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        measure.draw(image, in: CGRect(x: 0, y: 0, width: probe, height: probe))
        guard let pixels = measure.data?.assumingMemoryBound(to: UInt8.self) else { return nil }
        func alpha(_ x: Int, _ y: Int) -> Int { Int(pixels[(y * probe + x) * 4 + 3]) }
        var clear = 0
        var light = 0.0
        var left = probe, right = -1, top = probe, bottom = -1
        for y in 0..<probe {
            for x in 0..<probe {
                let index = y * probe + x
                let a = Double(pixels[index * 4 + 3]) / 255
                guard a >= 0.5 else { clear += 1; continue }
                left = min(left, x); right = max(right, x); top = min(top, y); bottom = max(bottom, y)
                let red = Double(pixels[index * 4]), green = Double(pixels[index * 4 + 1]), blue = Double(pixels[index * 4 + 2])
                light += (0.299 * red + 0.587 * green + 0.114 * blue) / 255 / a
            }
        }
        let solid = probe * probe - clear
        guard solid > 0 else { return nil }
        let last = probe - 1
        // The bitmap's first row is the picture's top row, as in a CGImage.
        let across = CGFloat(image.width) / CGFloat(probe), down = CGFloat(image.height) / CGFloat(probe)
        let box = CGRect(
            x: CGFloat(left) * across, y: CGFloat(top) * down,
            width: CGFloat(right - left + 1) * across, height: CGFloat(bottom - top + 1) * down
        ).integral.intersection(CGRect(x: 0, y: 0, width: image.width, height: image.height))
        return Shape(
            box: box,
            clearShare: Double(clear) / Double(probe * probe),
            isLight: light / Double(solid) > 0.8,
            cornersClear: [alpha(0, 0), alpha(last, 0), alpha(0, last), alpha(last, last)].allSatisfy { $0 < 128 }
        )
    }
}
