import Foundation

private func requireSendable<T: Sendable>(_: T) {}
private func requireHashable<T: Hashable>(_: T) {}

private func checkApiProtocols(_ owner: NativeSignetProtocol, _ credential: SignetCredentialProtocol) {
    _ = owner
    _ = credential.encoded()
}

private func checkApiValues() {
    let error = SignetError.InvalidInput
    requireHashable(error)
    requireSendable(error)
    _ = error.errorDescription
    let event = SignetEvent.failed(requestId: 0, error: error)
    requireSendable(event)
}
