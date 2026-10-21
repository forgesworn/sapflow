import Foundation

struct V4vHTTPCall: CustomStringConvertible, CustomDebugStringConvertible {
    let method: String
    let path: String
    let bearer: String?
    let body: Data?
    init(_ request: V4vControlRequest) {
        method = "POST"; path = request.operation.path; bearer = request.bearer; body = request.operation.bytes
    }
    init(status identity: V4vIdentity, bearer: String) {
        method = "GET"; path = "/api/live-events/\(identity.eventId)/native-session/\(identity.sessionId)"; self.bearer = bearer; body = nil
    }
    init(current event: String, bearer: String) throws {
        method = "GET"; path = "/api/live-events/\(try V4vIdentity.event(event))/native-session"; self.bearer = bearer; body = nil
    }
    init(loginChallenge: Void) {
        method = "GET"; path = "/api/auth/nostr/challenge"; bearer = nil; body = nil
    }
    init(login proof: V4vLoginProof) throws {
        method = "POST"; path = "/api/auth/nostr/login"; bearer = nil; body = try proof.body
    }
    func urlRequest(profile: V4vConnectorProfile) throws -> URLRequest {
        guard let url = URL(string: profile.apiOrigin + path) else { throw V4vFailure.invalidProfile }
        if let bearer {
            guard (1...8192).contains(bearer.utf8.count), bearer.unicodeScalars.allSatisfy({ $0.value >= 33 && $0.value <= 126 }) else { throw V4vFailure.unauthenticated }
        }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 8)
        request.httpMethod = method; request.httpBody = body
        request.httpShouldHandleCookies = false
        if let bearer { request.setValue("Bearer " + bearer, forHTTPHeaderField: "Authorization") }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        if body != nil { request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        return request
    }
    var description: String { "V4V HTTP call (redacted)" }
    var debugDescription: String { description }
}

/// Shared by the actual delegate and pure bound tests. Error/redirect bodies
/// never become JSON errors or text/logs containing server secrets.
struct V4vResponseBody {
    private(set) var bytes = Data()
    mutating func append(_ data: Data) throws {
        guard data.count <= V4vJSON.maxBody - bytes.count else { throw V4vFailure.bounds }
        bytes.append(data)
    }
    static func check(status: Int, expectedLength: Int64) throws {
        if status == 401 || status == 403 { throw V4vFailure.unauthenticated }
        if status == 409 { throw V4vFailure.conflict }
        guard status == 200 else { throw V4vFailure.unavailable }
        guard expectedLength <= V4vJSON.maxBody else { throw V4vFailure.bounds }
    }
}

/// One request, one original 8-second deadline, at most 64KiB buffered. The
/// platform's normal certificate+hostname validation is untouched. No cookies,
/// redirect following, cache, retries, background transfer or media fetches.
/// The future app owner must call close; this foundation is not attached to UI.
final class V4vControlClient: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let profile: V4vConnectorProfile
    private let queue = DispatchQueue(label: "sapflow.v4v.control.http")
    private var session: URLSession!
    private var task: URLSessionDataTask?
    private var timer: DispatchSourceTimer?
    private var completion: ((Result<V4vJSON, V4vFailure>) -> Void)?
    private var body = V4vResponseBody()
    private var acceptedResponse = false
    private var closed = false
    // Admission is bounded BEFORE dispatching, not merely after queueing. All
    // delegate state remains on queue; this lock protects only two cold flags.
    private let admission = NSLock()
    private var admitted = false
    private var admissionClosed = false
    private var invalidated = false
    private let onClosed: () -> Void
    /// Only an actual URLSession invalidation callback completes close. A
    /// cancel request alone does not release a retained observation owner.
    var cleanupComplete: Bool { admission.lock(); defer { admission.unlock() }; return admissionClosed && invalidated }
    init(profile: V4vConnectorProfile, onClosed: @escaping () -> Void = {}) {
        self.profile = profile; self.onClosed = onClosed
        super.init()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false; configuration.httpCookieStorage = nil; configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = 8; configuration.timeoutIntervalForResource = 8
        configuration.httpMaximumConnectionsPerHost = 1
        let delegateQueue = OperationQueue(); delegateQueue.maxConcurrentOperationCount = 1; delegateQueue.underlyingQueue = queue
        session = URLSession(configuration: configuration, delegate: self, delegateQueue: delegateQueue)
    }
    func execute(_ call: V4vHTTPCall, completion: @escaping (Result<V4vJSON, V4vFailure>) -> Void) {
        admission.lock()
        if admissionClosed || admitted {
            let failure: V4vFailure = admissionClosed ? .closed : .busy
            admission.unlock(); completion(.failure(failure)); return
        }
        admitted = true; admission.unlock()
        queue.async { [self] in
            guard !closed else { releaseAdmission(); completion(.failure(.closed)); return }
            do {
                let request = try call.urlRequest(profile: profile)
                body = V4vResponseBody(); acceptedResponse = false; self.completion = completion
                let next = session.dataTask(with: request); task = next
                let deadline = DispatchSource.makeTimerSource(queue: queue); timer = deadline
                deadline.schedule(deadline: .now() + 8)
                deadline.setEventHandler { [weak self, weak next] in
                    guard let self, self.task === next else { return }
                    self.finish(.failure(.unavailable))
                }
                deadline.resume(); next.resume()
            } catch { releaseAdmission(); completion(.failure(.invalidContract)) }
        }
    }
    func close() {
        admission.lock()
        guard !admissionClosed else { admission.unlock(); return }
        admissionClosed = true; admission.unlock()
        queue.async { [self] in
            guard !closed else { return }; closed = true
            finish(.failure(.closed)); session.invalidateAndCancel()
        }
    }
    private func releaseAdmission() { admission.lock(); admitted = false; admission.unlock() }
    private func finish(_ result: Result<V4vJSON, V4vFailure>) {
        timer?.setEventHandler {}; timer?.cancel(); timer = nil
        let callback = completion; completion = nil
        let old = task; task = nil; old?.cancel()
        body = V4vResponseBody(); acceptedResponse = false
        releaseAdmission()
        callback?(result)
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
        if self.task === task { finish(.failure(.unavailable)) }
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse, completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard task === dataTask, let response = response as? HTTPURLResponse, response.url == dataTask.originalRequest?.url else { completionHandler(.cancel); return }
        do {
            try V4vResponseBody.check(status: response.statusCode, expectedLength: response.expectedContentLength)
            acceptedResponse = true; completionHandler(.allow)
        } catch { completionHandler(.cancel); finish(.failure((error as? V4vFailure) ?? .unavailable)) }
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard task === dataTask else { return }
        do { try body.append(data) }
        catch { finish(.failure(.bounds)) }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard self.task === task else { return }
        guard error == nil, acceptedResponse else { finish(.failure(.unavailable)); return }
        do { finish(.success(try V4vJSON.parse(body.bytes))) }
        catch { finish(.failure((error as? V4vFailure) ?? .invalidContract)) }
    }
    func urlSession(_ session: URLSession, didBecomeInvalidWithError error: Error?) {
        admission.lock(); let first = !invalidated; invalidated = true; admission.unlock()
        if first { onClosed() }
    }
}
