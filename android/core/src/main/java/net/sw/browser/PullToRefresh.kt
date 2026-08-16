package net.sw.browser

import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration

/**
 * Drag the page down from the top to reload it.
 *
 * Written by hand rather than with SwipeRefreshLayout. That is a dependency the
 * offline build has no copy of, and it works by wrapping the view and
 * intercepting touches on its behalf — which would put it between the page and
 * every downward drag the page itself wants, on a map, a carousel, a canvas.
 * This only reads the events the activity already dispatches and consumes none
 * of them, so the page keeps every gesture it had. The cost is that the page
 * scrolls a little under the finger while the indicator is being pulled; the
 * page is already at its top, so there is nowhere for it to go.
 *
 * [atTop] is asked at the start of a gesture rather than continuously: a drag
 * that begins halfway down a page is a scroll, and it stays a scroll even once
 * it has reached the top.
 */
class PullToRefresh(
    private val indicator: View,
    private val atTop: () -> Boolean,
    private val onRefresh: () -> Unit,
) {

    private val density = indicator.resources.displayMetrics.density

    /** How far the finger travels for the indicator to reach the commit point. */
    private val distance = 96 * density

    /** Where the indicator rests while the page is reloading. */
    private val resting = 28 * density

    private val slop = ViewConfiguration.get(indicator.context).scaledTouchSlop

    private var startX = 0f
    private var startY = 0f

    /** The gesture began at the top of the page, so it may become a pull. */
    private var armed = false

    /** The gesture has been decided to be a pull rather than a scroll. */
    private var pulling = false

    /** A reload is in flight; the indicator is spinning and owns its position. */
    private var refreshing = false

    init {
        park()
    }

    /**
     * Whether a pull may start now.
     *
     * Set by the activity for the screens the browser is not in front of — the
     * library covers the page, and a drag there belongs to the list.
     */
    var enabled = true

    fun onTouch(event: MotionEvent) {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                startX = event.rawX
                startY = event.rawY
                pulling = false
                armed = enabled && !refreshing && atTop()
            }

            MotionEvent.ACTION_MOVE -> {
                if (!armed) return
                val dy = event.rawY - startY
                val dx = event.rawX - startX

                if (!pulling) {
                    // Downward, and clearly more downward than sideways, or the
                    // swipe that opens the panel would also pull the page.
                    if (dy < slop || dy < kotlin.math.abs(dx) * 1.2f) {
                        // Sideways first means this gesture is not ours. Give it
                        // up for good rather than reconsidering every sample —
                        // a finger that curves downward mid-swipe would
                        // otherwise start a pull halfway through a panel swipe.
                        if (kotlin.math.abs(dx) > slop) armed = false
                        return
                    }
                    pulling = true
                }
                show(dy)
            }

            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (!pulling) return
                pulling = false
                val dy = event.rawY - startY
                if (event.actionMasked == MotionEvent.ACTION_UP && dy >= distance) {
                    begin()
                } else {
                    retract()
                }
            }
        }
    }

    /**
     * Follow the finger, with the last part of the travel resisted.
     *
     * A one-to-one indicator reaches the commit point and then keeps going,
     * which reads as nothing having happened at the moment something did. The
     * square-root slows it as it approaches so the stop is felt.
     */
    private fun show(dy: Float) {
        val fraction = (dy / distance).coerceIn(0f, 1f)
        val eased = kotlin.math.sqrt(fraction.toDouble()).toFloat()
        indicator.visibility = View.VISIBLE
        indicator.alpha = fraction.coerceAtMost(1f)
        indicator.scaleX = 0.6f + 0.4f * eased
        indicator.scaleY = indicator.scaleX
        indicator.translationY = -indicator.height + eased * (resting + indicator.height)
        // A full turn over the pull, so the arrow is back where it started at
        // the moment the spin takes over and the two do not visibly disagree.
        indicator.rotation = fraction * 360f
    }

    private fun begin() {
        refreshing = true
        indicator.animate().cancel()
        indicator.alpha = 1f
        indicator.scaleX = 1f
        indicator.scaleY = 1f
        indicator.translationY = resting
        indicator.animate().rotationBy(360f).setDuration(700)
            .setInterpolator(android.view.animation.LinearInterpolator())
            .withEndAction(object : Runnable {
                override fun run() {
                    if (!refreshing) return
                    indicator.animate().rotationBy(360f).setDuration(700)
                        .setInterpolator(android.view.animation.LinearInterpolator())
                        .withEndAction(this).start()
                }
            }).start()
        // A page that never reports finishing would leave the arrow turning for
        // good, and nothing could be pulled again while it did.
        indicator.postDelayed({ finished() }, GIVE_UP_MS)
        onRefresh()
    }

    /**
     * The page has finished loading, so stop.
     *
     * Called for every load, not only the ones this started — a page that
     * navigates while the indicator is up should still put it away.
     */
    fun finished() {
        if (!refreshing) return
        refreshing = false
        indicator.animate().cancel()
        retract()
    }

    private fun retract() {
        indicator.animate()
            .translationY(-indicator.height.toFloat())
            .alpha(0f)
            .setDuration(180)
            .withEndAction { if (!refreshing) park() }
            .start()
    }

    /**
     * Out of sight above the top edge.
     *
     * Height is not known until layout, so the first park runs before there is
     * a number to use; `GONE` covers that moment, and every later park has a
     * real height and only needs the position.
     */
    private fun park() {
        indicator.visibility = View.GONE
        indicator.alpha = 0f
        indicator.rotation = 0f
        indicator.translationY = -indicator.height.toFloat()
    }

    private companion object {
        /** How long the arrow turns before giving up on being told to stop. */
        const val GIVE_UP_MS = 15_000L
    }
}
