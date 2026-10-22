package dev.forgesworn.sapflow.broadcaster

import java.util.concurrent.atomic.AtomicReference

/** One cold terminal receipt. An exact epoch registers once; epoch cleanup
 * never fires it. Native branch closure is a separate caller-owned receipt. */
internal class TerminalAdmissionSignal {
    private class State(val listener:(()->Unit)?=null,val terminal:Boolean=false)
    private val state=AtomicReference(State())
    fun listen(listener:()->Unit) {
        while(true) {
            val old=state.get()
            if(old.listener!=null)return
            val next=State(listener,old.terminal)
            if(state.compareAndSet(old,next)){if(old.terminal)listener();return}
        }
    }
    fun closeAdmission() {
        while(true) {
            val old=state.get()
            if(old.terminal)return
            val next=State(old.listener,true)
            if(state.compareAndSet(old,next)){old.listener?.invoke();return}
        }
    }
}
