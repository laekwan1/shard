import SwiftUI

/// The downloads in flight and the ones just finished — several at once, each
/// with its own progress and a cancel.
struct DownloadsView: View {
    @ObservedObject var store: DownloadsStore

    var body: some View {
        NavigationView {
            Group {
                if store.items.isEmpty {
                    Text("받는 중인 항목이 없습니다")
                        .foregroundColor(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List {
                        ForEach(store.items) { item in
                            row(item)
                        }
                    }
                }
            }
            .navigationTitle("다운로드")
            .toolbar {
                if store.items.contains(where: { $0.status != .running }) {
                    Button("지우기") { store.clearDone() }
                }
            }
        }
    }

    @ViewBuilder
    private func row(_ item: Download) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(item.title).lineLimit(1)
                Spacer()
                statusView(item)
            }
            if item.status == .running {
                ProgressView(value: item.fraction)
                HStack {
                    Text(progressText(item)).font(.caption).foregroundColor(.secondary)
                    Spacer()
                    Button("취소") { store.cancel(item.id) }.font(.caption)
                }
            }
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder
    private func statusView(_ item: Download) -> some View {
        switch item.status {
        case .running:
            Text("\(Int(item.fraction * 100))%").font(.caption).foregroundColor(.secondary)
        case .finished:
            Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
        case .cancelled:
            Text("취소됨").font(.caption).foregroundColor(.secondary)
        case .failed(let message):
            Text(message).font(.caption).foregroundColor(.red).lineLimit(1)
        }
    }

    private func progressText(_ item: Download) -> String {
        let done = ByteCountFormatter.string(fromByteCount: Int64(item.done), countStyle: .file)
        guard item.total > 0 else { return done }
        let total = ByteCountFormatter.string(fromByteCount: Int64(item.total), countStyle: .file)
        return "\(done) / \(total)"
    }
}
