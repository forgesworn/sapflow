import SwiftUI
import AVFoundation

final class ManualQrPreviewView: UIView {
    override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
    var previewLayer: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
}

private struct ManualQrPreview: UIViewRepresentable {
    let scanner: ManualQrScanner, selection: ManualQrScanner.Selection
    func makeUIView(context: Context) -> ManualQrPreviewView {
        let view = ManualQrPreviewView()
        view.previewLayer.videoGravity = .resizeAspectFill
        scanner.attachPreview(view, selection: selection)
        return view
    }
    func updateUIView(_ uiView: ManualQrPreviewView, context: Context) {}
    static func dismantleUIView(_ uiView: ManualQrPreviewView, coordinator: ()) {
        uiView.previewLayer.session = nil
        // View removal alone is not the session/delegate or modal receipt.
    }
}

/// Kept by exact presentation identity until the actual dismissal receipt.
struct ManualQrPresentation: View {
    @ObservedObject var scanner: ManualQrScanner
    let selection: ManualQrScanner.Selection
    var body: some View {
        Color.clear.frame(width: 0, height: 0)
            .sheet(isPresented: Binding(get: { scanner.isPresented(selection) },
                set: { if !$0 { scanner.requestDismissal(selection) } }),
                onDismiss: { scanner.dismissed(selection) }) {
                VStack(spacing: 16) {
                    Text("Scan a manual destination QR code").font(.headline)
                    Text("Scanning only offers an invitation. Review and Apply remain separate actions.")
                        .font(.caption)
                    ManualQrPreview(scanner: scanner, selection: selection)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                    if let message = scanner.message { Text(message).font(.caption) }
                    Button("Cancel", role: .cancel) { scanner.cancel(selection) }
                }.padding().privacySensitive()
            }
    }
}
