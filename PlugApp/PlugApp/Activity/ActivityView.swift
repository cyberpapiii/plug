import PlugIPC
import SwiftUI

struct ActivityView: View {
    let model: AppModel
    @State private var failuresOnly = false

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(
                title: "Activity",
                subtitle: "Recent calls through Plug",
                metrics: [
                    (String(model.activities.count), "Calls"),
                    (String(failureCount), "Issues")
                ]
            )
            if filteredActivities.isEmpty {
                ContentUnavailableView(
                    failuresOnly ? "No recent issues" : "No recent activity",
                    systemImage: failuresOnly ? "checkmark.circle" : "waveform.path.ecg",
                    description: Text(failuresOnly ? "Recent calls completed successfully." : "Tool calls will appear here as clients use Plug.")
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Table(filteredActivities) {
                    TableColumn("Method", value: \.method)
                    TableColumn("Client") { OptionalActivityValue(value: $0.client) }
                    TableColumn("Server") { OptionalActivityValue(value: $0.server) }
                    TableColumn("Time") { Text("\($0.latencyMs) ms").monospacedDigit() }
                    TableColumn("Result") { ActivityResultLabel(outcome: $0.outcome) }
                }
            }
        }
        .navigationTitle("Activity")
        .toolbar {
            Toggle("Issues only", isOn: $failuresOnly)
                .toggleStyle(.button)
        }
    }

    private var filteredActivities: [PlugIPC.ActivityEvent] {
        model.activities.filter { !failuresOnly || $0.outcome != "success" }
    }

    private var failureCount: Int {
        model.activities.filter { $0.outcome != "success" }.count
    }
}

private struct OptionalActivityValue: View {
    let value: String?

    var body: some View {
        if let value { Text(value) }
        else { Text("—").foregroundStyle(.secondary) }
    }
}

private struct ActivityResultLabel: View {
    let outcome: String

    var body: some View {
        if outcome == "success" {
            Label("Success", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.secondary)
        } else {
            Label(outcome.capitalized, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
        }
    }
}
