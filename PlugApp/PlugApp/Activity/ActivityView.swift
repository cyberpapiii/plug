import SwiftUI

struct ActivityView: View {
    let model: AppModel
    @State private var failuresOnly = false
    var body: some View {
        Table(model.activities.filter { !failuresOnly || $0.outcome != "success" }) {
            TableColumn("Method", value: \.method)
            TableColumn("Client") { Text($0.client ?? "—") }
            TableColumn("Server") { Text($0.server ?? "—") }
            TableColumn("Time") { Text("\($0.latencyMs) ms") }
            TableColumn("Result", value: \.outcome)
        }
        .navigationTitle("Activity")
        .toolbar { Toggle("Failures only", isOn: $failuresOnly) }
    }
}
