import lab.signet.*

private class SmokeFailure(val code: String) : Exception(code)
private fun requireThat(condition: Boolean, code: String) { if (!condition) throw SmokeFailure(code) }

private const val persona = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
private const val uri = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=wss://relay.example.invalid"
private val credentialBytes = "{\"version\":1,\"persona\":\"$persona\",\"app_secret_key\":\"${"1".repeat(64)}\",\"bunker_uri\":\"$uri\"}".toByteArray()

private inline fun <reified T : SignetException> expect(block: () -> Unit) {
    try { block() } catch (error: SignetException) {
        if (error is T) return
        throw SmokeFailure("wrong_error")
    }
    throw SmokeFailure("refusal_missing")
}

private fun close(owner: NativeSignet) {
    owner.requestClose()
    requireThat(owner.waitClosed(2_000u), "close_timeout")
    requireThat(owner.isClosed(), "not_closed")
    owner.close()
}

private fun run() {
    val input = credentialBytes.copyOf()
    val credential = SignetCredential.restore(input)
    input.fill(0)
    val output = credential.encoded()
    requireThat(output.contentEquals(credentialBytes), "credential_input_alias")
    output.fill(0)
    requireThat(credential.encoded().contentEquals(credentialBytes), "credential_output_alias")
    expect<SignetException.InvalidCredential> { SignetCredential.restore("{}".toByteArray()) }
    expect<SignetException.InvalidInput> { NativeSignet.pairBunker(uri, 0u) }
    expect<SignetException.InvalidInput> { NativeSignet.pairBunker(uri, 120_001u) }
    expect<SignetException.InvalidInput> { NativeSignet.pairBunker(uri.replace("wss:", "ws:"), 1_000u) }

    val owner = NativeSignet.pairBunker(uri, 120_000u)
    expect<SignetException.Busy> { NativeSignet.pairBunker(uri, 1_000u) }
    expect<SignetException.InvalidInput> { owner.connect(0uL) }
    expect<SignetException.InvalidInput> {
        owner.signLogin(1uL, "a".repeat(64), "https://fixture.example.invalid", "Generated offline fixture", 1uL)
    }
    expect<SignetException.InvalidInput> { owner.waitEvent(1_001u) }
    expect<SignetException.InvalidInput> { owner.waitClosed(2_001u) }
    owner.requestClose()
    requireThat(owner.waitClosed(2_000u), "close_timeout")
    requireThat(owner.isClosed(), "not_closed")
    expect<SignetException.Closed> { owner.connect(1uL) }
    close(owner)

    val restored = NativeSignet.restore(credential, 120_000u)
    close(restored)

    val expiring = NativeSignet.pairBunker(uri, 30u)
    var event: SignetEvent? = null
    val until = System.nanoTime() + 2_000_000_000L
    while (event == null && System.nanoTime() < until) event = expiring.waitEvent(100u)
    requireThat(event is SignetEvent.Failed, "deadline_event")
    event as SignetEvent.Failed
    requireThat(event.requestId == 0uL && event.error is SignetException.Deadline, "deadline_fields")
    requireThat(expiring.waitClosed(2_000u), "deadline_close")
    requireThat(expiring.waitEvent(0u) == null, "event_replay")
    expect<SignetException.Closed> { expiring.connect(1uL) }
    expiring.close()

    val replacement = NativeSignet.pairBunker(uri, 120_000u)
    close(replacement)
    credential.close()
}

fun main() {
    try {
        run()
        println("kotlin-api-smoke: ok")
    } catch (error: Throwable) {
        System.err.println("kotlin-api-smoke: $error")
        kotlin.system.exitProcess(1)
    }
}
