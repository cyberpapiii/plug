import SwiftUI

struct SidebarSectionRow: View {
    let section: AppSection
    let detail: String

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: section.symbol)
                .foregroundStyle(.secondary)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(section.rawValue)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
    }
}

struct RuntimeFooter: View {
    let model: AppModel

    var body: some View {
        HStack(spacing: 8) {
            StatusDot(color: statusColor)
            VStack(alignment: .leading, spacing: 1) {
                Text(statusTitle).font(.caption.weight(.medium))
                if !model.snapshot.runtimeVersion.isEmpty {
                    Text("Version \(model.snapshot.runtimeVersion)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(.bar)
    }

    private var statusColor: Color {
        if model.connectionState != .ready { return .red }
        return model.isHealthy ? .green : .orange
    }

    private var statusTitle: String {
        if model.connectionState != .ready { return "Not connected" }
        return model.isHealthy ? "Running normally" : "Needs attention"
    }
}

struct ServiceAdoptionNotice: View {
    let model: AppModel

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "checkmark.shield")
                .font(.title3)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text("Finish setup").fontWeight(.semibold)
                Text("Let this app keep Plug running in the background.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Use Plug") { Task { await model.adoptDaemon() } }
                .buttonStyle(.borderedProminent)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.regularMaterial)
        .overlay(alignment: .bottom) { Divider() }
    }
}

struct PageHeader: View {
    let title: String
    let subtitle: String
    let metrics: [(String, String)]

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 18) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.title2.weight(.semibold))
                Text(subtitle).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            ForEach(Array(metrics.enumerated()), id: \.offset) { _, metric in
                VStack(alignment: .trailing, spacing: 1) {
                    Text(metric.0).font(.headline.monospacedDigit())
                    Text(metric.1).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .background(.background)
        .overlay(alignment: .bottom) { Divider() }
    }
}

struct StatusDot: View {
    let color: Color

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 8, height: 8)
            .overlay(Circle().stroke(.white.opacity(0.35), lineWidth: 0.5))
    }
}

struct EmptySectionRow: View {
    let title: String
    let systemImage: String

    var body: some View {
        Label(title, systemImage: systemImage)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 8)
    }
}

struct ErrorToast: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.triangle.fill")
            .font(.callout)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
            .shadow(radius: 8, y: 2)
            .padding()
    }
}
