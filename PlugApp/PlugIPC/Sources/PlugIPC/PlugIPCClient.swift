import Darwin
import Foundation

public actor PlugIPCClient {
    private var descriptor: Int32 = -1
    private let socketURL: URL
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(socketURL: URL = PlugIPCClient.defaultSocketURL) {
        self.socketURL = socketURL
        encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
    }

    public static var defaultSocketURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/plug/plug.sock")
    }

    public static var defaultTokenURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/plug/plug.token")
    }

    public func connect() throws -> OperatorHandshake {
        if descriptor < 0 { descriptor = try Self.openSocket(path: socketURL.path) }
        let response = try request(.handshake(clientVersion: "0.5.3", ipcMin: 3, ipcMax: 4))
        guard case let .handshake(handshake) = response else { throw PlugIPCError.unexpectedResponse("handshake") }
        return handshake
    }

    public func request(_ request: IPCRequest) throws -> IPCResponse {
        if descriptor < 0 { descriptor = try Self.openSocket(path: socketURL.path) }
        do {
            try Self.writeAll(FrameCodec.encode(request, encoder: encoder), to: descriptor)
            let header = try Self.readExact(4, from: descriptor)
            let length = header.reduce(UInt32.zero) { ($0 << 8) | UInt32($1) }
            guard length <= FrameCodec.maximumPayloadSize else { throw PlugIPCError.frameTooLarge(Int(length)) }
            let payload = try Self.readExact(Int(length), from: descriptor)
            let response = try decoder.decode(IPCResponse.self, from: payload)
            if case let .error(code, message) = response { throw PlugIPCError.daemon(code, message) }
            return response
        } catch {
            disconnect()
            throw error
        }
    }

    public func disconnect() {
        if descriptor >= 0 { Darwin.close(descriptor); descriptor = -1 }
    }

    static func openSocket(path: String) throws -> Int32 {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw PlugIPCError.systemCall("socket", errno) }
        var (address, addressLength) = try unixSocketAddress(path: path)
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, addressLength)
            }
        }
        guard result == 0 else { let code = errno; Darwin.close(fd); throw PlugIPCError.systemCall("connect", code) }
        return fd
    }

    static func unixSocketAddress(path: String) throws -> (sockaddr_un, socklen_t) {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw PlugIPCError.socketPathTooLong
        }
        withUnsafeMutableBytes(of: &address.sun_path) { destination in
            bytes.withUnsafeBytes { source in
                destination.copyBytes(from: source)
            }
        }
        let headerLength = MemoryLayout<sockaddr_un>.offset(of: \sockaddr_un.sun_path)!
        let addressLength = headerLength + bytes.count
        address.sun_len = UInt8(addressLength)
        return (address, socklen_t(addressLength))
    }

    private static func writeAll(_ data: Data, to fd: Int32) throws {
        try data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let count = Darwin.write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                guard count > 0 else { throw PlugIPCError.systemCall("write", errno) }
                offset += count
            }
        }
    }

    private static func readExact(_ count: Int, from fd: Int32) throws -> Data {
        var data = Data(count: count)
        try data.withUnsafeMutableBytes { raw in
            var offset = 0
            while offset < count {
                let readCount = Darwin.read(fd, raw.baseAddress!.advanced(by: offset), count - offset)
                guard readCount > 0 else { throw readCount == 0 ? PlugIPCError.disconnected : PlugIPCError.systemCall("read", errno) }
                offset += readCount
            }
        }
        return data
    }
}
