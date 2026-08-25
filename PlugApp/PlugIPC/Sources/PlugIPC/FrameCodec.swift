import Foundation

public enum FrameCodec {
    public static let maximumPayloadSize = 4 * 1024 * 1024

    public static func encode<T: Encodable>(_ value: T, encoder: JSONEncoder = JSONEncoder()) throws -> Data {
        let payload = try encoder.encode(value)
        guard payload.count <= maximumPayloadSize else { throw PlugIPCError.frameTooLarge(payload.count) }
        var length = UInt32(payload.count).bigEndian
        var frame = withUnsafeBytes(of: &length) { Data($0) }
        frame.append(payload)
        return frame
    }

    public static func decode<T: Decodable>(_ type: T.Type, from frame: Data, decoder: JSONDecoder = JSONDecoder()) throws -> T {
        guard frame.count >= 4 else { throw PlugIPCError.truncatedFrame }
        let length = frame.prefix(4).reduce(UInt32.zero) { ($0 << 8) | UInt32($1) }
        guard length <= maximumPayloadSize else { throw PlugIPCError.frameTooLarge(Int(length)) }
        guard frame.count == Int(length) + 4 else { throw PlugIPCError.truncatedFrame }
        return try decoder.decode(type, from: frame.dropFirst(4))
    }
}

public enum PlugIPCError: LocalizedError, Equatable {
    case frameTooLarge(Int)
    case truncatedFrame
    case socketPathTooLong
    case timedOut
    case disconnected
    case systemCall(String, Int32)
    case daemon(String, String)
    case unexpectedResponse(String)

    public var errorDescription: String? {
        switch self {
        case .frameTooLarge: "Plug sent an unexpectedly large response."
        case .truncatedFrame: "Plug closed the connection mid-response."
        case .socketPathTooLong: "Plug's local socket path is too long."
        case .timedOut: "Plug did not respond before the local IPC deadline."
        case let .systemCall(name, code): "\(name) failed (\(code))."
        case .disconnected: "Plug is not running."
        case let .daemon(_, message): message
        case let .unexpectedResponse(type): "Plug returned an unexpected response: \(type)."
        }
    }
}
