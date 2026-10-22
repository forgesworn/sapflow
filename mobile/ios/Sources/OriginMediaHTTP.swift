import Foundation

protocol OriginMediaFetching: AnyObject {
    var cleanupComplete: Bool { get }
    func whenCleanupComplete(_ completion: @escaping () -> Void)
    func fetch(_ resources: OriginApprovedResources, deadlineNs: UInt64, completion: @escaping (Result<Data, OriginObservationFailure>) -> Void)
    func close()
}

/// Dedicated normal-TLS media client. It cannot receive an API bearer and
/// never shares cookies, request headers or a session with creator control.
/// One pair is admitted; a stalled delegate cannot accumulate queued probes.
final class OriginMediaHTTP: NSObject, URLSessionDataDelegate, OriginMediaFetching, @unchecked Sendable {
    private let queue = DispatchQueue(label: "sapflow.origin.media.http")
    private let admission = NSLock()
    private var admitted = false, admissionClosed = false
    private var invalidated = false
    private let cleanup = OriginCleanupNotification()
    var cleanupComplete: Bool { admission.lock(); defer { admission.unlock() }; return admissionClosed && invalidated }
    func whenCleanupComplete(_ completion: @escaping () -> Void) { cleanup.observe(completion) }
    private var session: URLSession!
    private var task: URLSessionDataTask?
    private var deadline: DispatchSourceTimer?
    private var completion: ((Result<Data, OriginObservationFailure>) -> Void)?
    private var resources: OriginApprovedResources?
    private var body: OriginMediaBody?
    private var initialization = Data()
    private var accepted = false, fetchingPart = false, closed = false
    private var limitNs: UInt64 = 0
    private let now: () -> UInt64
    init(now: @escaping () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }) {
        self.now = now
        super.init()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false; configuration.httpCookieStorage = nil; configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = 8; configuration.timeoutIntervalForResource = 8
        configuration.httpMaximumConnectionsPerHost = 1
        let delegateQueue = OperationQueue(); delegateQueue.maxConcurrentOperationCount = 1; delegateQueue.underlyingQueue = queue
        session = URLSession(configuration: configuration, delegate: self, delegateQueue: delegateQueue)
    }
    func fetch(_ resources: OriginApprovedResources, deadlineNs: UInt64, completion: @escaping (Result<Data, OriginObservationFailure>) -> Void) {
        admission.lock()
        guard !admitted, !admissionClosed else {
            let error: OriginObservationFailure = admissionClosed ? .cancelled : .busy
            admission.unlock(); completion(.failure(error)); return
        }
        admitted = true; admission.unlock()
        queue.async { [self] in
            guard !closed else { finish(.failure(.cancelled), fallback: completion); return }
            let now = self.now()
            guard deadlineNs > now, deadlineNs - now <= 8_000_000_000 else { finish(.failure(.deadline), fallback: completion); return }
            self.completion = completion; self.resources = resources; limitNs = deadlineNs
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now() + .nanoseconds(Int(deadlineNs - now)))
            timer.setEventHandler { [weak self] in self?.finish(.failure(.deadline)) }
            deadline = timer; timer.resume()
            start(resources.initialization, limit: OriginMediaBody.initLimit)
        }
    }
    private func start(_ url: URL, limit: Int) {
        do {
            let now = self.now()
            guard now < limitNs, let resources else { throw OriginObservationFailure.deadline }
            let request = try resources.request(url, timeout: min(8, Double(limitNs - now) / 1_000_000_000))
            body = try OriginMediaBody(limit: limit); accepted = false
            let next = session.dataTask(with: request); task = next; next.resume()
        } catch { finish(.failure((error as? OriginObservationFailure) ?? .network)) }
    }
    func close() {
        admission.lock()
        guard !admissionClosed else { admission.unlock(); return }
        admissionClosed = true; admission.unlock()
        queue.async { [self] in
            closed = true; finish(.failure(.cancelled)); session.invalidateAndCancel()
        }
    }
    private func finish(_ result: Result<Data, OriginObservationFailure>, fallback: ((Result<Data, OriginObservationFailure>) -> Void)? = nil) {
        deadline?.setEventHandler {}; deadline?.cancel(); deadline = nil
        let callback = completion ?? fallback; completion = nil
        let old = task; task = nil; old?.cancel()
        resources = nil; body = nil; initialization = Data(); accepted = false; fetchingPart = false
        admission.lock(); admitted = false; admission.unlock()
        callback?(result)
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
        if self.task === task { finish(.failure(.network)) }
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse, completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard task === dataTask, let response = response as? HTTPURLResponse, response.url == dataTask.originalRequest?.url, let body else { completionHandler(.cancel); return }
        do {
            try OriginMediaBody.response(status: response.statusCode, length: response.expectedContentLength, mime: response.mimeType, limit: body.limit)
            accepted = true; completionHandler(.allow)
        } catch { completionHandler(.cancel); finish(.failure((error as? OriginObservationFailure) ?? .network)) }
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard task === dataTask else { return }
        do { try body?.append(data) }
        catch { finish(.failure(.bounds)) }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard self.task === task else { return }
        guard error == nil, accepted, let bytes = body?.bytes, !bytes.isEmpty, now() < limitNs else { finish(.failure(.network)); return }
        self.task = nil
        if !fetchingPart {
            initialization = bytes; fetchingPart = true
            guard let resources else { finish(.failure(.scope)); return }
            start(resources.part, limit: OriginMediaBody.totalLimit - bytes.count)
        } else {
            var combined = initialization; combined.append(bytes)
            finish(.success(combined))
        }
    }
    func urlSession(_ session: URLSession, didBecomeInvalidWithError error: Error?) {
        admission.lock(); invalidated = true; admission.unlock()
        cleanup.complete()
    }
}
