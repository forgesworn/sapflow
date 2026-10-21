package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/** One borrowed handle already owned by the fanout consumer, never a queue.
 * process holds only bounded protocol work, never socket IO. Cancellation waits
 * for that bounded use before allowing the consumer to release its handle. */
internal class SingleSampleOffer<T> {
    class Ticket<T>(val value: T, val deadlineUs: Long) {
        val done = CountDownLatch(1)
        var cancelled = false // synchronised on ticket
    }
    private val pending = AtomicReference<Ticket<T>?>()
    fun offer(value: T, deadlineUs: Long): Ticket<T> {
        val ticket = Ticket(value, deadlineUs)
        check(pending.compareAndSet(null, ticket)) { "SRT single-sample admission bound exceeded" }
        return ticket
    }
    fun deadlineUs(): Long? = pending.get()?.deadlineUs
    fun process(nowUs: Long, action: (T) -> Boolean) {
        val ticket = pending.get() ?: return
        synchronized(ticket) {
            if (ticket.cancelled) return
            check(nowUs < ticket.deadlineUs) { "SRT media admission expired" }
            if (action(ticket.value)) {
                pending.compareAndSet(ticket, null)
                ticket.done.countDown()
            }
        }
    }
    fun await(ticket: Ticket<T>, timeoutMs: Long): Boolean {
        if (!ticket.done.await(timeoutMs, TimeUnit.MILLISECONDS)) {
            synchronized(ticket) {
                ticket.cancelled = true
                pending.compareAndSet(ticket, null)
                ticket.done.countDown()
            }
            return false
        }
        return synchronized(ticket) { !ticket.cancelled }
    }
    fun cancel() {
        val ticket = pending.getAndSet(null) ?: return
        synchronized(ticket) { ticket.cancelled = true; ticket.done.countDown() }
    }
}
