package dev.forgesworn.sapflow.broadcaster

fun main() {
    val uncapped = PreviewAdmission(0)
    check((0 until 30).all { uncapped.claim(it.toLong()) })
    val policy = PreviewAdmission(15)
    val encoded = mutableListOf<Long>()
    val previews = mutableListOf<Long>()
    for (frame in 0 until 30) {
        val now = frame * 1_000_000_000L / 30
        encoded += now // encoder admission precedes the separate preview claim
        if (policy.claim(now)) previews += now
    }
    check(encoded == (0 until 30).map { it * 1_000_000_000L / 30 })
    check(previews.size == 15) { "expected 15 preview frames, got ${previews.size}" }
    val longRun = PreviewAdmission(15)
    var admitted = 0
    for (frame in 0 until 18_000) {
        val now = frame * 1_000_000_000L / 30
        if (longRun.claim(now)) admitted++
    }
    check(admitted == 9_000) { "ten-minute count $admitted" }
    val jitter = PreviewAdmission(15)
    check(jitter.claim(0)); check(!jitter.claim(66_000_000)); check(jitter.claim(67_000_000))
    check(!jitter.claim(100_000_000)); check(jitter.claim(134_000_000))
    val stalled = PreviewAdmission(15)
    check(stalled.claim(0)); check(stalled.claim(1_000_000_000)); check(!stalled.claim(1_000_000_001))
    check(stalled.claim(1_066_666_666))
    stalled.reset(); check(stalled.claim(1_066_666_667)) // new surface/source
    check(!stalled.claim(1_066_666_668)); stalled.reset(); check(stalled.claim(1_066_666_669))
    check(stalled.claim(5)) // clock regression starts a new epoch
    val edge = PreviewAdmission(15)
    check(edge.claim(Long.MAX_VALUE - 5)); check(edge.claim(Long.MAX_VALUE - 4))
    check(edge.claim(Long.MIN_VALUE + 1))
    check(runCatching { PreviewAdmission(30) }.isFailure)
    println("PASS: default, 30 fps input, jitter, stalled no-catch-up, reset and clock regression")
}
