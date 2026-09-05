package net.sw.browser

import android.app.Activity
import android.content.ClipData
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.DragEvent
import android.view.LayoutInflater
import android.view.View
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.PopupMenu
import android.widget.TextView
import android.widget.Toast
import androidx.annotation.OptIn
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.VideoSize
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.SeekParameters
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import java.util.concurrent.Executors

/**
 * The offline library, as a screen laid over the browser.
 *
 * A screen rather than a page: what has been saved has nothing to do with what
 * is loaded, and a back press has to land somewhere predictable. This covers
 * the browser completely and back returns to it — the web view is left exactly
 * as it was, still on whatever it was showing.
 *
 * Nothing is cached. The list is asked for from storage each time the screen is
 * opened, because a file manager can delete a video without telling this app,
 * and a row that opens nothing is worse than a row that is not there.
 */
@OptIn(UnstableApi::class)
class LibraryScreen(private val activity: Activity, parent: ViewGroup) {

    private val work = Executors.newSingleThreadExecutor()
    private val ui = Handler(Looper.getMainLooper())

    private val root: View =
        LayoutInflater.from(activity).inflate(R.layout.view_library, parent, false).also {
            parent.addView(it)
        }

    private val stage: FrameLayout = root.findViewById(R.id.stage)
    private val surface: SurfaceView = root.findViewById(R.id.player)
    private val expand: ImageButton = root.findViewById(R.id.expand)
    private val leaveFullScreen: ImageButton = root.findViewById(R.id.leaveFullScreen)
    private val chrome: View = root.findViewById(R.id.libraryChrome)
    private val header: View = root.findViewById(R.id.libraryHeader)
    private val folderRow: LinearLayout = root.findViewById(R.id.folders)
    private val folderScroll: View = root.findViewById(R.id.folderScroll)
    private val kinds: android.widget.FrameLayout = root.findViewById(R.id.kinds)
    private val shelfThumb: View = root.findViewById(R.id.shelfThumb)
    private val tabVideo: View = root.findViewById(R.id.tabVideo)
    private val tabVideoIcon: android.widget.ImageView = root.findViewById(R.id.tabVideoIcon)
    private val tabVideoText: TextView = root.findViewById(R.id.tabVideoText)
    private val tabMusic: View = root.findViewById(R.id.tabMusic)
    private val tabMusicIcon: android.widget.ImageView = root.findViewById(R.id.tabMusicIcon)
    private val tabMusicText: TextView = root.findViewById(R.id.tabMusicText)
    private val gauge: View = root.findViewById(R.id.gauge)
    private val gaugeIcon: android.widget.ImageView = root.findViewById(R.id.gaugeIcon)
    private val gaugeBar: android.widget.ProgressBar = root.findViewById(R.id.gaugeBar)
    private val stageArt: android.widget.ImageView = root.findViewById(R.id.stageArt)
    private val videoBar: View = root.findViewById(R.id.videoBar)
    private val vbSeek: android.widget.SeekBar = root.findViewById(R.id.vbSeek)
    private val vbPrev: ImageButton = root.findViewById(R.id.vbPrev)
    private val vbToggle: ImageButton = root.findViewById(R.id.vbToggle)
    private val vbNext: ImageButton = root.findViewById(R.id.vbNext)
    private val vbElapsed: TextView = root.findViewById(R.id.vbElapsed)
    private val vbTotal: TextView = root.findViewById(R.id.vbTotal)
    private val stageTitle: TextView = root.findViewById(R.id.stageTitle)
    private val vbMute: ImageButton = root.findViewById(R.id.vbMute)
    private val stageClose: ImageButton = root.findViewById(R.id.stageClose)
    private val vbRate: TextView = root.findViewById(R.id.vbRate)
    private val folderSettings: ImageButton = root.findViewById(R.id.folderSettings)
    private val nowPlaying: View = root.findViewById(R.id.nowPlaying)
    private val npPrev: ImageButton = root.findViewById(R.id.npPrev)
    private val npNext: ImageButton = root.findViewById(R.id.npNext)
    private val npArt: com.google.android.material.imageview.ShapeableImageView =
        root.findViewById(R.id.npArt)
    private val npTitle: TextView = root.findViewById(R.id.npTitle)
    private val npToggle: ImageButton = root.findViewById(R.id.npToggle)
    private val npStop: ImageButton = root.findViewById(R.id.npStop)
    private val npMute: ImageButton = root.findViewById(R.id.npMute)
    private val npSeek: android.widget.SeekBar = root.findViewById(R.id.npSeek)
    private val npElapsed: TextView = root.findViewById(R.id.npElapsed)
    private val npTotal: TextView = root.findViewById(R.id.npTotal)
    private val list: ListView = root.findViewById(R.id.saved)
    private val empty: View = root.findViewById(R.id.libraryEmpty)
    private val emptyIcon: android.widget.ImageView = root.findViewById(R.id.emptyIcon)
    private val emptyText: TextView = root.findViewById(R.id.emptyText)

    private var items: List<Library.Item> = emptyList()
    private var folders: List<String> = emptyList()

    /** Which shelf is showing: videos or music. */
    private var tab: Library.Kind = Library.Kind.VIDEO

    /** Which folder is being shown, or null for everything. */
    private var showing: String? = null

    /** What is on the stage, so it can be named and acted on. */
    private var playing: Library.Item? = null

    private var expanded = false

    val isOpen: Boolean get() = root.visibility == View.VISIBLE

    /**
     * Told when the library comes up and when it goes back to the browser.
     *
     * The browser uses these to pause the page's own playback while the library
     * is over it — a video and a song should not both be playing — and to let it
     * carry on from where it paused once the library is left.
     */
    var onOpen: (() -> Unit)? = null
    var onClose: (() -> Unit)? = null

    private val adapter = object : BaseAdapter() {
        override fun getCount() = shown().size
        override fun getItem(at: Int) = shown()[at]
        // Asked for rows that have just gone. Between a shelf changing and the
        // view laying itself out again, the ListView still believes the old
        // count and asks about positions the new shelf does not have. The list
        // is the truth; a missing row answers with no id rather than throwing.
        override fun getItemId(at: Int) = shown().getOrNull(at)?.id ?: -1L

        override fun getView(at: Int, reuse: View?, parent: ViewGroup): View {
            val view = reuse ?: LayoutInflater.from(activity)
                .inflate(R.layout.item_saved, parent, false).also { roundArt(it) }
            val item = shown()[at]
            // The separator lives in the row, so the last row hides its own —
            // a line under the final item rules the list into a table.
            view.findViewById<View>(R.id.rowLine).visibility =
                if (at == shown().size - 1) View.INVISIBLE else View.VISIBLE
            view.findViewById<TextView>(R.id.name).text = item.title
            view.findViewById<TextView>(R.id.detail).text = describe(item)
            // The one fact that says whether a file is worth opening now. Zero
            // means nothing read it — a badge saying 0:00 would be a lie, so
            // there is no badge.
            val length = view.findViewById<TextView>(R.id.length)
            if (item.durationMs > 0) {
                length.text = clock(item.durationMs.toInt())
                length.visibility = View.VISIBLE
            } else {
                length.visibility = View.GONE
            }
            bindArt(
                view.findViewById(R.id.art),
                view.findViewById(R.id.playBadge),
                item,
            )
            view.findViewById<ImageButton>(R.id.more).setOnClickListener { anchor ->
                showMenu(anchor, item)
            }
            // The row answers its own tap.
            //
            // Not through the list's item click: giving a row a long-press
            // listener makes it handle touches itself, and a row handling
            // touches never lets the list see the tap. Carrying a row to a
            // folder and opening it are the same view's business, so both live
            // here.
            view.setOnClickListener { play(item) }
            // Held down, a row can be carried to a folder along the top. The
            // menu still does the same thing; this is the way that reads as
            // putting something somewhere rather than choosing from a list.
            view.setOnLongClickListener { dragged ->
                val label = ClipData.newPlainText("shard-item", item.name)
                dragged.startDragAndDrop(label, View.DragShadowBuilder(dragged), item, 0)
                true
            }
            // Dropped on a folder along the top, a row goes into it; dropped on
            // another row, it takes that row's place. The two gestures are told
            // apart by what is underneath when the finger lifts, which is the
            // only thing the user was aiming at.
            view.setOnDragListener { row, event ->
                val carried = event.localState as? Library.Item ?: return@setOnDragListener false
                when (event.action) {
                    DragEvent.ACTION_DRAG_ENTERED -> if (carried.id != item.id) row.alpha = 0.6f
                    DragEvent.ACTION_DRAG_EXITED, DragEvent.ACTION_DRAG_ENDED -> row.alpha = 1f
                    DragEvent.ACTION_DROP -> {
                        row.alpha = 1f
                        // Above or below, by which half of the row it landed on.
                        if (carried.id != item.id) {
                            rearrange(carried, item, event.y < row.height / 2f)
                        }
                    }
                }
                true
            }
            return view
        }
    }

    // ---- thumbnails --------------------------------------------------------
    //
    // A saved file's own picture, so the list is scanned by eye. The media store
    // renders these on request; kept small and remembered once made, so a scroll
    // does not ask for the same frame twice, and read off the main thread the
    // same as anything else that touches storage.

    // Newest request first. A fling binds every row it passes; ordering the
    // queue last-in-first-out means the row the finger stops on is decoded next,
    // not after the hundred that flew by — and the threads run at background
    // priority so the decoding never steals the frames the scroll itself needs.
    private val thumbWork = java.util.concurrent.ThreadPoolExecutor(
        2, 2, 0L, java.util.concurrent.TimeUnit.MILLISECONDS,
        object : java.util.concurrent.LinkedBlockingDeque<Runnable>() {
            override fun offer(e: Runnable): Boolean = offerFirst(e)
        },
    )
    private val thumbCache = android.util.LruCache<android.net.Uri, android.graphics.Bitmap>(48)
    // The same rung `tile.xml` uses, read from the ladder rather than typed
    // again: this one is applied in code because a ShapeableImageView takes its
    // shape from a model, not from a background drawable.
    private val tileCorner = activity.resources.getDimension(R.dimen.r_md)

    /**
     * The corner on the list row's frame: the bottom rung.
     *
     * A radius is only right against the side it is cut from. 12 on the 46dp
     * square in the music bar reads as a rounded tile; on a frame 36 tall it
     * was a third of the height and the pictures came out as lozenges. Even 8
     * still read as a pill at that height — a corner keeps reading as a curve
     * until it is a clearly small share of the side, and on 36dp that is 4.
     * The desktop's own 26px frame sits on the same rung.
     */
    private val shotCorner = activity.resources.getDimension(R.dimen.r_xs)
    // Sized to the shorter side of the 64x36 frame. At the 14dp it was drawn for
    // — a 52dp square — the glyph came out taller than the frame it sits in.
    private val tilePad = (8 * activity.resources.displayMetrics.density).toInt()
    private val mutedTint =
        android.content.res.ColorStateList.valueOf(activity.getColor(R.color.muted))

    /** Round the tile once, when the row is first inflated rather than every bind. */
    private fun roundArt(row: View) {
        val art = row.findViewById<com.google.android.material.imageview.ShapeableImageView>(R.id.art)
        art.shapeAppearanceModel =
            art.shapeAppearanceModel.toBuilder().setAllCornerSizes(shotCorner).build()
    }

    private fun bindArt(
        art: com.google.android.material.imageview.ShapeableImageView,
        badge: android.widget.ImageView?,
        item: Library.Item,
    ) {
        // The row is recycled; the uri says which file this view is for now — and
        // is unique across the two collections where an id is not — so a thumbnail
        // that finishes late for a scrolled-away row is dropped.
        art.tag = item.uri
        val music = item.kind == Library.Kind.MUSIC

        val cached = thumbCache.get(item.uri)
        if (cached != null) {
            showThumb(art, badge, cached, music)
            return
        }
        showTilePlaceholder(art, badge, music)
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        thumbWork.execute {
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_BACKGROUND)
            // A song carries no frame of its own. The picture inside the file is
            // the first place to look — that is where a download puts it, and
            // where every music app looks — then the one remembered beside it,
            // from before songs carried their own.
            var bmp = if (music) embeddedArt(item) else null
            if (bmp == null && music) bmp = Covers.load(activity, Covers.keyFor(item.name))
            // A real frame at the video's OWN aspect, so a 9:16 short comes back tall instead
            // of cropped to 16:9. loadThumbnail returns the requested (landscape) shape, which
            // is what made shorts look wide.
            if (bmp == null && !music) bmp = videoFrame(item)
            if (bmp == null) {
                bmp = runCatching {
                    activity.contentResolver.loadThumbnail(
                        item.uri, android.util.Size(320, 180), null
                    )
                }.getOrNull()
            }
            if (bmp == null) return@execute
            thumbCache.put(item.uri, bmp)
            ui.post {
                if (art.tag == item.uri) showThumb(art, badge, bmp, music)
                // The row that kicked off this decode is not always the row on screen for the
                // file: opening the library the ListView binds a throwaway row just to measure a
                // row height, and that row's async frame would land on a view that is never shown
                // while the real row kept its placeholder — the small centred icon. That is the
                // "first thumbnail is small until you leave the folder and come back" bug: the
                // frame WAS decoded and cached, so a later rebind hit the cache and filled. Paint
                // the file into whatever visible row shows it now and it fills on first sight.
                paintVisibleThumb(item.uri, bmp, music)
            }
        }
    }

    /**
     * Paint an already-decoded frame into the visible list row for a file, if one is up.
     *
     * A companion to the captured-view fast path above, for when the view that started a
     * decode is not the one now on screen (the ListView's throwaway measurement row, on the
     * first open). Only the handful of on-screen children are scanned — no full relayout that
     * would fight a scroll — and it is a no-op when nothing visible is showing this file.
     */
    private fun paintVisibleThumb(uri: android.net.Uri, bmp: android.graphics.Bitmap, music: Boolean) {
        for (i in 0 until list.childCount) {
            val row = list.getChildAt(i) ?: continue
            val art = row.findViewById<com.google.android.material.imageview.ShapeableImageView>(R.id.art)
                ?: continue
            if (art.tag == uri) showThumb(art, row.findViewById(R.id.playBadge), bmp, music)
        }
    }

    /** Rename where the name is, which is the only place it means anything. */
    private fun askForName(item: Library.Item) {
        val box = android.widget.EditText(activity).apply {
            setText(item.title)
            setSelection(text.length)
            setSingleLine()
        }
        MaterialAlertDialogBuilder(activity)
            .setTitle(R.string.library_rename)
            .setView(box)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.confirm) { _, _ ->
                val wanted = box.text.toString()
                work.execute {
                    val done = Library.rename(activity, item, wanted)
                    ui.post { if (done) reload() else toast(activity.getString(R.string.library_rename_failed)) }
                }
            }
            .show()
    }

    /**
     * Put one file before or after another and remember the whole shelf.
     *
     * The whole shelf, not the part being shown: a list narrowed to one folder
     * is still a slice of one order, and saving only the slice would forget
     * where everything else stood.
     */
    private fun rearrange(carried: Library.Item, target: Library.Item, above: Boolean) {
        val shelf = Library.arranged(activity, items.filter { it.kind == tab }, tab).toMutableList()
        val from = shelf.indexOfFirst { it.id == carried.id }
        if (from < 0) return
        shelf.removeAt(from)
        var to = shelf.indexOfFirst { it.id == target.id }
        if (to < 0) to = shelf.size else if (!above) to += 1
        shelf.add(to, carried)
        Library.setOrder(activity, tab, shelf.map { it.name })
        adapter.notifyDataSetChanged()
    }

    /**
     * Ask a file to start where it is now, or to forget that.
     *
     * On the row's own menu rather than on the picture: the phone has no second
     * button, and a long press on the stage is already the gesture that carries
     * a row somewhere.
     */
    private fun holdHere(item: Library.Item) {
        val at = player?.currentPosition ?: 0L
        if (playing?.id != item.id || at <= 1_000) {
            toast(activity.getString(R.string.library_hold_needs_playing))
            return
        }
        Library.setHold(activity, item, at)
        toast(activity.getString(R.string.library_hold_saved, clock(at.toInt())))
    }

    private fun forgetHold(item: Library.Item) {
        Library.clearHold(activity, item)
        toast(activity.getString(R.string.library_hold_cleared))
    }

    /**
     * The picture a file carries in its own header.
     *
     * Android reads it out for us, so this is the platform's own answer to
     * "what is this song's cover" rather than a second copy of the parsing that
     * put it there.
     */
    private fun embeddedArt(item: Library.Item): android.graphics.Bitmap? = runCatching {
        val reader = android.media.MediaMetadataRetriever()
        try {
            reader.setDataSource(activity, item.uri)
            val bytes = reader.embeddedPicture ?: return null
            android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
        } finally {
            runCatching { reader.release() }
        }
    }.getOrNull()

    /** A frame from the video at its OWN aspect. getScaledFrameAtTime fits within the box
     *  keeping the ratio, so a vertical short returns tall — shown CENTER_INSIDE it reads as a
     *  short, not a middle slice cropped to landscape. A few seconds in, past the opening black. */
    private fun videoFrame(item: Library.Item): android.graphics.Bitmap? = runCatching {
        val reader = android.media.MediaMetadataRetriever()
        try {
            reader.setDataSource(activity, item.uri)
            val durUs = (reader.extractMetadata(
                android.media.MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull() ?: 0L) * 1000
            val at = if (durUs > 6_000_000) minOf(3_000_000L, durUs / 3) else 0L
            reader.getScaledFrameAtTime(
                at, android.media.MediaMetadataRetriever.OPTION_CLOSEST_SYNC, 320, 320)
        } finally {
            runCatching { reader.release() }
        }
    }.getOrNull()

    /** The tile before a thumbnail lands: a type mark centred on the well. */
    private fun showTilePlaceholder(
        art: com.google.android.material.imageview.ShapeableImageView,
        badge: android.widget.ImageView?,
        music: Boolean,
    ) {
        art.scaleType = android.widget.ImageView.ScaleType.CENTER_INSIDE
        art.setPadding(tilePad, tilePad, tilePad, tilePad)
        art.imageTintList = mutedTint
        art.setImageResource(if (music) R.drawable.ic_music else R.drawable.ic_video)
        badge?.visibility = View.GONE
    }

    /** The tile once a real frame or album art is in: filled, with a play mark over a video. */
    private fun showThumb(
        art: com.google.android.material.imageview.ShapeableImageView,
        badge: android.widget.ImageView?,
        bmp: android.graphics.Bitmap,
        music: Boolean,
    ) {
        art.imageTintList = null
        art.setPadding(0, 0, 0, 0)
        // Video: FIT_CENTER keeps aspect (a short stays tall) AND scales to the frame — unlike
        // CENTER_INSIDE, which leaves a small thumbnail small, so frames of different source
        // resolutions showed at different sizes. Now every video thumbnail fills the frame's
        // height consistently. Music: album art is square, CENTER_CROP fills rather than
        // sitting in side bars.
        val scale = if (music) android.widget.ImageView.ScaleType.CENTER_CROP
                    else android.widget.ImageView.ScaleType.FIT_CENTER
        art.scaleType = scale
        art.setImageBitmap(bmp)
        badge?.visibility = if (music) View.GONE else View.VISIBLE
        // FIT_CENTER sizes the picture from the view's bounds, and at the very first bind — the
        // first time the library opens — the row is not laid out yet (width 0), so the frame came
        // out shrunk and only a later relayout (leaving the folder and coming back) filled it (the
        // "first thumbnails don't fill" bug). When the view has no size yet, re-apply once it does.
        // Clearing the drawable first forces the matrix to be recomputed — re-setting the same
        // bitmap alone is a no-op because the drawable is unchanged.
        if (art.width == 0 || art.height == 0) {
            art.post {
                art.scaleType = scale
                art.setImageDrawable(null)
                art.setImageBitmap(bmp)
            }
        }
    }

    // ---- the music bar ----------------------------------------------------

    /** Whether the scrubber is being dragged, so the ticker leaves it alone. */
    private var npSeeking = false

    /** Which song the bar is currently made up for, so it is not rebuilt on every look. */
    private var npFor: android.net.Uri? = null

    /**
     * Bring up the top-of-screen player for a song and fill it in.
     *
     * Filled in once per song: coming back to the music shelf while the same one
     * is still playing must not reset the scrubber to the beginning, which is
     * what rebuilding it every time would look like.
     */
    private fun showNowPlaying(item: Library.Item) {
        // The sound glyph on the music bar reflects the level too.
        syncVolumeBar()
        if (npFor != item.uri) {
            npFor = item.uri
            npTitle.text = item.title
            npArt.shapeAppearanceModel =
                npArt.shapeAppearanceModel.toBuilder().setAllCornerSizes(tileCorner).build()
            bindArt(npArt, null, item)
            npSeek.progress = 0
            npElapsed.text = clock(0)
            npTotal.text = clock(0)
        }
        nowPlaying.visibility = View.VISIBLE
        updateNowPlaying()
        ui.removeCallbacks(npTick)
        ui.post(npTick)
    }

    /** The bar's button follows the intent, the same as the notification's. */
    private fun updateNowPlaying() {
        npToggle.setImageResource(
            if (prepared && wantsPlay) R.drawable.ic_pb_pause else R.drawable.ic_pb_play
        )
    }

    private fun hideNowPlaying() {
        ui.removeCallbacks(npTick)
        nowPlaying.visibility = View.GONE
    }

    /** Move the scrubber and clocks along while a song plays. */
    private val npTick = object : Runnable {
        override fun run() {
            val mp = player
            if (mp != null && prepared && !npSeeking && playing?.kind == Library.Kind.MUSIC) {
                val duration = mp.duration.coerceAtLeast(1)
                val position = mp.currentPosition.coerceIn(0, duration)
                npSeek.progress = (position * 1000 / duration).toInt()
                npElapsed.text = clock(position.toInt())
                npTotal.text = clock(duration.toInt())
            }
            ui.postDelayed(this, 500)
        }
    }

    /** Milliseconds as m:ss. */
    private fun clock(ms: Int): String {
        val seconds = (ms / 1000).coerceAtLeast(0)
        return "%d:%02d".format(seconds / 60, seconds % 60)
    }

    /**
     * Play, pause and a scrub bar, from the framework's own controller driving
     * our player.
     *
     * Anchored to the stage rather than to the window: anchored to the window
     * it appears at the bottom of the screen, a long way from the picture it
     * belongs to, and over the list.
     */

    /** Set once the screen is let go, so nothing drives it afterwards. */
    private var released = false

    /**
     * The stop hook this screen puts in the process-global bridge.
     *
     * Kept as a field so it can be recognised on the way out: only the screen
     * that installed a hook removes it, which matters if a config change ever
     * builds a second screen before the first is released. Nulling it in
     * release() is also what stops the global from pinning this whole screen —
     * and through it the activity, the session and the view tree — alive for the
     * life of the process the foreground service keeps running.
     */
    private val stopHook: () -> Unit = { ui.post { if (!released) stopPlaying() } }
    private val toggleHook: () -> Unit = { ui.post { if (!released) togglePlayPause() } }

    // ---- the video bar -----------------------------------------------------
    //
    // In place of the framework's MediaController, which drew white-and-lavender
    // Material defaults over a screen that is black and cyan everywhere else and
    // offers no way to restyle it — it builds its own views. It also had room
    // for nothing but play and seek, which is what pushed the rest of a player's
    // controls into a menu beside the new-folder button and a button floating
    // over the corner of the picture.

    /** Whether the bar over the picture is up. */
    private var controlsShown = false

    // The shared motion ladder — the same three durations and curves the desktop
    // shell keeps in `app.css`, read from resources so there is one place to
    // change them. See `res/values/motion.xml`.
    private val tSwap = activity.resources.getInteger(R.integer.t_swap).toLong()
    private val easeEnter =
        android.view.animation.AnimationUtils.loadInterpolator(activity, R.interpolator.ease_enter)
    private val easeExit =
        android.view.animation.AnimationUtils.loadInterpolator(activity, R.interpolator.ease_exit)

    /**
     * Bring a control in, or take it out, on the shared curve.
     *
     * The bar used to appear and vanish outright. Over a moving picture that
     * reads as a glitch rather than as something arriving — there is nothing to
     * tell the eye that the bar came from anywhere. Arriving and leaving get
     * different curves because they are different events: one settles, the other
     * gets out of the way.
     */
    private fun fade(view: View, on: Boolean) {
        view.animate().cancel()
        if (on) {
            if (view.visibility != View.VISIBLE) view.alpha = 0f
            view.visibility = View.VISIBLE
            view.animate().alpha(1f).setDuration(tSwap).setInterpolator(easeEnter).start()
        } else {
            if (view.visibility != View.VISIBLE) {
                view.alpha = 0f
                return
            }
            view.animate().alpha(0f).setDuration(tSwap).setInterpolator(easeExit)
                .withEndAction { view.visibility = View.GONE }.start()
        }
    }

    /** True while the video scrubber is under a finger, so the ticker leaves it. */
    private var vbSeeking = false


    /**
     * Put the bar away again after a while.
     *
     * The bar covers the bottom of the picture, so it goes on its own — the
     * same as the framework's controller did, and the same as every video app.
     */
    private val hideControls = Runnable { showControls(false) }

    /**
     * Start the countdown over, and redraw what the press just changed.
     *
     * A press can land on the bar while it is fading out. Alpha does not stop
     * touches, so for the 200ms the fade takes, the buttons are still there to
     * be hit — and `controlsShown` has already been cleared. Bailing on that
     * flag meant such a press toggled playback but left the glyph showing the
     * old state, did not stop the fade, and let the bar vanish out from under
     * the finger that had just used it.
     *
     * So the question is not "is the flag set" but "is the bar still on
     * screen". If it is, a press is a request to have it back.
     */
    private fun keepControlsUp() {
        if (!controlsShown && videoBar.visibility != View.VISIBLE) return
        // Cancels the fade, puts the alpha back, repaints, and restarts the
        // countdown — everything this has to do is what showing it does.
        showControls(true)
    }

    /**
     * Show or hide the bar, and with it the way out of full screen.
     *
     * The back arrow at the top left travels with the bar rather than standing
     * there always: full screen is a picture, and a pair of buttons parked over
     * the corners of it are two things permanently in the way.
     */
    private fun showControls(on: Boolean) {
        // Only a video has this bar. A song is driven from the bar under its
        // cover, and putting this one over the cover as well would be two sets
        // of the same buttons on one screen.
        if (on && playing?.kind != Library.Kind.VIDEO) return
        controlsShown = on
        ui.removeCallbacks(hideControls)
        fade(videoBar, on)
        // The title on the picture rides in and out with the bar below it.
        fade(stageTitle, on)
        fade(leaveFullScreen, on && expanded)
        // Close sits at the picture's top-right, up whenever the controls are.
        fade(stageClose, on)
        if (on) {
            updateVideoBar()
            ui.removeCallbacks(vbTick)
            ui.post(vbTick)
            // Only while something is actually playing. A stopped picture is
            // being looked at rather than watched, and taking the controls away
            // from someone who paused to use them is taking them away at the
            // one moment they are wanted. Pressing play starts the countdown,
            // because that press comes back through here.
            if (wantsPlay) ui.postDelayed(hideControls, CONTROLS_MS.toLong())
        } else {
            ui.removeCallbacks(vbTick)
        }
    }

    /** Move the video scrubber and clock along while the bar is up. */
    private val vbTick = object : Runnable {
        override fun run() {
            val exo = player
            if (exo != null && prepared && playing?.kind == Library.Kind.VIDEO) {
                val duration = exo.duration.coerceAtLeast(1)
                val position = exo.currentPosition.coerceIn(0, duration)
                if (!vbSeeking) vbSeek.progress = (position * 1000 / duration).toInt()
                vbElapsed.text = clock(position.toInt())
                vbTotal.text = clock(duration.toInt())
            }
            ui.postDelayed(this, TICK_MS)
        }
    }

    /**
     * Redraw the bar's buttons from the state they stand for.
     *
     * The play glyph follows the intent, not the instant — the same value the
     * notification's button is drawn from. Reading the player's transient state
     * would let the button flicker, or read as paused for as long as a seek
     * takes.
     */
    private fun updateVideoBar() {
        vbToggle.setImageResource(
            if (prepared && wantsPlay) R.drawable.ic_pb_pause else R.drawable.ic_pb_play
        )
        stageTitle.text = playing?.title.orEmpty()
        syncVolumeBar()
        vbRate.text = rateLabel(player?.playbackParameters?.speed ?: 1f)
        paintPlaybackToggles()
    }

    /** Draw the mute glyph from the system music level — crossed out at zero. */
    private fun syncVolumeBar() {
        val audio = activity.getSystemService(android.media.AudioManager::class.java) ?: return
        val now = audio.getStreamVolume(android.media.AudioManager.STREAM_MUSIC)
        val glyph = if (now == 0) R.drawable.ic_muted else R.drawable.ic_volume
        vbMute.setImageResource(glyph)
        npMute.setImageResource(glyph)
    }

    private var volumePopup: android.widget.PopupWindow? = null

    /** A volume slider off the sound icon; the low end is mute. */
    private fun showVolumeSlider(anchor: View) {
        volumePopup?.dismiss()
        val audio = activity.getSystemService(android.media.AudioManager::class.java) ?: return
        val most = audio.getStreamMaxVolume(android.media.AudioManager.STREAM_MUSIC).coerceAtLeast(1)
        val view = activity.layoutInflater.inflate(R.layout.popup_volume, null)
        val slider = view.findViewById<android.widget.SeekBar>(R.id.volumeSlider)
        slider.progress = audio.getStreamVolume(android.media.AudioManager.STREAM_MUSIC) * 1000 / most
        slider.setOnSeekBarChangeListener(object : android.widget.SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(bar: android.widget.SeekBar, progress: Int, fromUser: Boolean) {
                if (!fromUser) return
                audio.setStreamVolume(android.media.AudioManager.STREAM_MUSIC, progress * most / 1000, 0)
                syncVolumeBar()
                ui.removeCallbacks(hideControls)
            }
            override fun onStartTrackingTouch(bar: android.widget.SeekBar) { ui.removeCallbacks(hideControls) }
            override fun onStopTrackingTouch(bar: android.widget.SeekBar) { keepControlsUp() }
        })
        val popup = android.widget.PopupWindow(
            view,
            android.view.ViewGroup.LayoutParams.WRAP_CONTENT,
            android.view.ViewGroup.LayoutParams.WRAP_CONTENT,
            true,
        )
        popup.elevation = 12f
        popup.setOnDismissListener { volumePopup = null; keepControlsUp() }
        volumePopup = popup
        ui.removeCallbacks(hideControls)
        // Just above the icon. It is measured first so the upward offset is its
        // real height, not a guess.
        view.measure(
            View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
            View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
        )
        popup.showAsDropDown(anchor, 0, -(view.measuredHeight + anchor.height), android.view.Gravity.END)
    }

    private fun rateLabel(rate: Float): String {
        // No trailing zero: 1.5× not 1.50×, and 1× not 1.0×.
        val s = if (rate == rate.toLong().toFloat()) rate.toLong().toString()
        else rate.toString().trimEnd('0').trimEnd('.')
        return s + "×"
    }

    /** The speeds the rate button steps through, the same set the desktop uses. */
    private val rates = listOf(1f, 1.25f, 1.5f, 2f, 0.5f, 0.75f)

    /** The speeds off the rate button — laid out left to right, above it, the
     *  same shape as the sound slider. */
    private fun showRateMenu() {
        val exo = player ?: return
        val view = activity.layoutInflater.inflate(R.layout.popup_rates, null)
        val choices = listOf(
            R.id.rate05 to 0.5f,
            R.id.rate075 to 0.75f,
            R.id.rate10 to 1f,
            R.id.rate125 to 1.25f,
            R.id.rate15 to 1.5f,
            R.id.rate20 to 2f,
        )
        val current = exo.playbackParameters.speed
        val popup = android.widget.PopupWindow(
            view,
            android.view.ViewGroup.LayoutParams.WRAP_CONTENT,
            android.view.ViewGroup.LayoutParams.WRAP_CONTENT,
            true,
        )
        popup.elevation = 12f
        for ((id, rate) in choices) {
            val cell = view.findViewById<TextView>(id)
            // The one in force is lit.
            cell.setTextColor(
                activity.getColor(
                    if (kotlin.math.abs(rate - current) < 0.01f) R.color.accent else R.color.on_surface
                )
            )
            cell.setOnClickListener {
                exo.setPlaybackSpeed(rate)
                vbRate.text = rateLabel(rate)
                keepControlsUp()
                popup.dismiss()
            }
        }
        popup.setOnDismissListener { keepControlsUp() }
        ui.removeCallbacks(hideControls)
        view.measure(
            View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
            View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
        )
        popup.showAsDropDown(vbRate, 0, -(view.measuredHeight + vbRate.height), android.view.Gravity.END)
    }

    /**
     * The three settings that used to live behind the menu, lit when on.
     *
     * Colour is the whole of the state here, so it has to be the difference
     * between on and off rather than a shade of it: the accent against the
     * muted grey the rest of the bar is drawn in.
     */
    private fun paintPlaybackToggles() {
        // The three switches live only in the gear popup now — on both the video
        // and the music player — so their lit state is the popup's to draw.
        paintPlaybackPopup()
    }

    /** The gear popup's rows carry the same lit state, while it is open. */
    private fun paintPlaybackPopup() {
        val views = playbackPopup?.contentView ?: return
        val end = Library.playbackEnd(activity)
        val background = Library.backgroundPlayback(activity)
        for ((id, on) in listOf(
            R.id.pbOnwardIcon to (end == Library.PlaybackEnd.NEXT),
            R.id.pbShuffleIcon to (end == Library.PlaybackEnd.SHUFFLE),
            R.id.pbBackgroundIcon to background,
        )) {
            views.findViewById<android.widget.ImageView>(id).imageTintList =
                android.content.res.ColorStateList.valueOf(
                    activity.getColor(if (on) R.color.accent else R.color.muted)
                )
        }
    }

    /** The gear popup, kept so its icons can be re-tinted while it is open. */
    private var playbackPopup: android.widget.PopupWindow? = null

    /** Open the end-of-file settings off the folder-row gear. */
    private fun showPlaybackSettings() {
        val view = activity.layoutInflater.inflate(R.layout.popup_playback, null)
        val popup = android.widget.PopupWindow(
            view,
            android.view.ViewGroup.LayoutParams.WRAP_CONTENT,
            android.view.ViewGroup.LayoutParams.WRAP_CONTENT,
            true,
        )
        playbackPopup = popup
        view.findViewById<View>(R.id.pbOnward).setOnClickListener {
            chooseEnd(Library.PlaybackEnd.NEXT)
        }
        view.findViewById<View>(R.id.pbShuffle).setOnClickListener {
            chooseEnd(Library.PlaybackEnd.SHUFFLE)
        }
        view.findViewById<View>(R.id.pbBackground).setOnClickListener {
            Library.setBackgroundPlayback(activity, !Library.backgroundPlayback(activity))
            paintPlaybackToggles()
        }
        popup.setOnDismissListener { playbackPopup = null }
        popup.elevation = 12f
        paintPlaybackPopup()
        // Below the gear; the window nudges itself left to stay on screen since
        // the gear sits at the right edge.
        popup.showAsDropDown(folderSettings, 0, 8, android.view.Gravity.END)
    }

    /**
     * Turn one of the end-of-file settings on, or off again.
     *
     * They are one setting with three values, so lighting one puts the other
     * out and pressing the lit one goes back to stopping. Two switches over a
     * single choice have to say so, or turning on "in order" would leave "at
     * random" looking on as well.
     */
    private fun chooseEnd(want: Library.PlaybackEnd) {
        val now = Library.playbackEnd(activity)
        Library.setPlaybackEnd(activity, if (now == want) Library.PlaybackEnd.STOP else want)
        paintPlaybackToggles()
    }

    /**
     * Step through the queue by hand.
     *
     * Back means "start this one again" once it is under way, and only reaches
     * the one before when it has barely begun — what every player does, and
     * what a hand reaching for it without looking expects.
     */
    private fun step(forward: Boolean) {
        val current = playing ?: return
        val list = queueFor(current)
        val at = list.indexOfFirst { it.id == current.id }
        if (at < 0 || list.isEmpty()) return
        // Wrap round at the ends, the same as the desktop does: 이전 on the first
        // file goes to the last, 다음 on the last goes to the first. Previous
        // means the previous file — it used to restart the current one after a
        // few seconds, which read as the button being broken.
        val to = ((if (forward) at + 1 else at - 1) + list.size) % list.size
        play(list[to])
    }

    init {
        // Held down on what is playing: where it should start from next time.
        //
        // On the picture rather than in the row's menu — it is about this moment
        // in this file, and the moment is on screen while the finger is on it.
        stage.setOnLongClickListener {
            val item = playing ?: return@setOnLongClickListener false
            MaterialAlertDialogBuilder(activity)
                .setTitle(item.title)
                .setItems(
                    arrayOf(
                        activity.getString(R.string.library_hold),
                        activity.getString(R.string.library_hold_forget),
                    ),
                ) { _, which -> if (which == 0) holdHere(item) else forgetHold(item) }
                .show()
            true
        }

        // Below the status bar, not under it.
        //
        // The browser pads itself for the bars; this screen lies over the top of
        // it and was never told, so its title was drawn through the clock. Asked
        // for rather than measured: full screen hides the bars, and then the
        // inset is nought and the picture reaches the edges by itself.
        androidx.core.view.ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(
                androidx.core.view.WindowInsetsCompat.Type.systemBars() or
                    androidx.core.view.WindowInsetsCompat.Type.displayCutout()
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }

        // The surface comes and goes with the window. The player does not, so
        // it is handed the new one each time rather than being rebuilt.
        // ExoPlayer is handed the surface view itself and looks after attaching
        // to it and repainting the frame when it comes back; this only keeps the
        // picture letterboxed to the right shape as the surface is laid out.
        surface.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) = fitSurface()
            override fun surfaceChanged(holder: SurfaceHolder, f: Int, w: Int, h: Int) = fitSurface()
            override fun surfaceDestroyed(holder: SurfaceHolder) {}
        })
        // The screen stays awake while a video is on the stage, and only then:
        // the stage is gone for a song, so this holds nothing while music plays.
        surface.keepScreenOn = true
        stage.setOnTouchListener { _, event ->
            // Pinching first: a two-finger gesture handed to the swipe detector
            // reads as a swipe by whichever finger moved further.
            pinch.onTouchEvent(event)
            // An interactive pull-to-web takes priority: while a rightward drag
            // is following the finger, the tap/scroll/fling detector is held off
            // so the two do not both act on the same move.
            val pulled = !pinch.isInProgress && handlePull(event)
            if (!pinch.isInProgress && !pulled) gestures.onTouchEvent(event)
            // The long-press starts a hold; the finger lifting is the only thing
            // that ends it, and the gesture detector does not report the lift.
            val a = event.actionMasked
            if (a == android.view.MotionEvent.ACTION_UP || a == android.view.MotionEvent.ACTION_CANCEL) endHold()
            true
        }
        EngineControl.stopPlayback = stopHook
        // The notification's play/pause button reaches the player through here.
        EngineControl.togglePlayback = toggleHook
        root.findViewById<ImageButton>(R.id.backFromLibrary).setOnClickListener { back() }
        expand.setOnClickListener { setExpanded(!expanded) }
        leaveFullScreen.setOnClickListener { setExpanded(false) }
        tabVideo.setOnClickListener { selectTab(Library.Kind.VIDEO) }
        tabMusic.setOnClickListener { selectTab(Library.Kind.MUSIC) }
        npToggle.setOnClickListener { togglePlayPause() }
        npStop.setOnClickListener { stopPlaying() }
        stageClose.setOnClickListener { stopPlaying() }
        npSeek.setOnSeekBarChangeListener(object : android.widget.SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(bar: android.widget.SeekBar, progress: Int, fromUser: Boolean) {
                if (!fromUser || !prepared) return
                val duration = player?.duration ?: return
                val at = progress.toLong() * duration / 1000
                // Exactly where it was dragged to — ExoPlayer seeks to the frame.
                player?.seekTo(at)
                npElapsed.text = clock(at.toInt())
            }
            override fun onStartTrackingTouch(bar: android.widget.SeekBar) { npSeeking = true }
            override fun onStopTrackingTouch(bar: android.widget.SeekBar) { npSeeking = false }
        })

        // The video bar. Every press that changes something puts its countdown
        // back to the start — a button pressed and the bar vanishing half a
        // second later reads as the press having dismissed it.
        vbToggle.setOnClickListener { togglePlayPause(); keepControlsUp() }
        vbPrev.setOnClickListener { step(forward = false); keepControlsUp() }
        vbNext.setOnClickListener { step(forward = true); keepControlsUp() }
        npPrev.setOnClickListener { step(forward = false) }
        npNext.setOnClickListener { step(forward = true) }
        // In order, at random and background all live under the gear on the
        // folder row now — off both the video and the music bar — so the two
        // players still answer to one decision.
        folderSettings.setOnClickListener { showPlaybackSettings() }
        vbSeek.setOnSeekBarChangeListener(object : android.widget.SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(bar: android.widget.SeekBar, progress: Int, fromUser: Boolean) {
                if (!fromUser || !prepared) return
                val duration = player?.duration ?: return
                val at = progress.toLong() * duration / 1000
                player?.seekTo(at)
                vbElapsed.text = clock(at.toInt())
            }
            override fun onStartTrackingTouch(bar: android.widget.SeekBar) {
                vbSeeking = true
                // The drag may have begun while the bar was fading out, in
                // which case bringing it back is what stops the fade — removing
                // the callback alone would leave the animation running and take
                // the line away mid-drag. Order matters: showing reposts the
                // countdown, and a finger on the line should have none.
                showControls(true)
                ui.removeCallbacks(hideControls)
            }
            override fun onStopTrackingTouch(bar: android.widget.SeekBar) {
                vbSeeking = false
                keepControlsUp()
            }
        })

        // A tap on the sound icon brings up a tall slider; dragging it sets the
        // level, and the bottom of it is zero — mute. The level is the system
        // music stream, the same one the side-drag and the hardware keys move.
        vbMute.setOnClickListener { showVolumeSlider(vbMute) }
        npMute.setOnClickListener { showVolumeSlider(npMute) }
        // The speed: a tap opens the set to pick from, rather than stepping one
        // at a time. Not kept between files — a rate is for the thing being
        // watched now.
        vbRate.setOnClickListener { showRateMenu() }
        paintPlaybackToggles()

        list.adapter = adapter
        // Rows carry their own click; see `getView`.
        selectTab(Library.Kind.VIDEO)
    }

    /**
     * Switch shelf, between videos and music.
     *
     * Folders belong to the video shelf, so the row of them and the button that
     * makes them go with it; music is a flat list and shows neither. The folder
     * filter is dropped on the way so a music shelf is never left narrowed to a
     * video folder it has nothing in.
     */
    private fun selectTab(kind: Library.Kind) {
        // Leaving a shelf while something plays: the player view goes either way
        // — a song's bar and a video's stage belong to the shelf they started on.
        // With background playback on the sound carries on unseen; with it off,
        // switching shelves stops it.
        if (kind != tab && playing != null && !Library.backgroundPlayback(activity)) {
            // Background playback off: leaving the shelf is leaving playback.
            // With it on, the sound carries on unseen and comes back with the
            // shelf — which is what `syncPlayerView` below settles.
            stopPlaying()
        }
        tab = kind
        showing = null
        syncPlayerView()
        styleTab(tabVideo, tabVideoIcon, tabVideoText, kind == Library.Kind.VIDEO)
        styleTab(tabMusic, tabMusicIcon, tabMusicText, kind == Library.Kind.MUSIC)
        moveShelfThumb(kind)
        // Both shelves keep folders now — the names differ per shelf, so they are
        // recomputed for the one being shown.
        folders = runCatching { Library.folders(activity, items, kind) }.getOrDefault(emptyList())
        drawFolders()
        adapter.notifyDataSetChanged()
        // Back to the top of the new shelf — but only if it has a top.
        //
        // A ListView told to select row 0 asks the adapter for row 0's id even
        // when the shelf is empty, and the adapter answers by reading the list,
        // which throws and takes the app with it. It only happens leaving a
        // shelf that had rows for one that has none, because the view's own
        // count is a layout behind the adapter's — which is why an empty
        // library opens fine and tapping 음악 on it does not.
        if (shown().isNotEmpty()) list.setSelection(0)
        showEmpty()
    }

    /**
     * A shelf tab in the segmented control, lit when it is the one in force.
     *
     * The pill background follows the selected state on its own; this colours
     * the label and its icon — the accent when in force, muted when not — so the
     * active shelf reads at a glance without shouting a full accent fill.
     */
    /**
     * Slide the lifted pill under the shelf that was chosen.
     *
     * Sized and placed here rather than in the layout: both depend on the
     * track's measured width, which does not exist until layout has run — the
     * `post` covers the first call, made from init before any layout. Jumped
     * rather than animated on that first call, because a pill seen sliding into
     * a screen that has only just appeared is answering a press nobody made.
     */
    init {
        // The thumb's size is a share of the track's, so a track that changes
        // width — a rotation — has to re-derive it. The listener re-runs the
        // same placement the tab switch does.
        kinds.addOnLayoutChangeListener { _, l, _, r, _, ol, _, or_, _ ->
            if (r - l != or_ - ol) moveShelfThumb(tab)
        }
    }

    private fun moveShelfThumb(kind: Library.Kind) {
        kinds.post {
            val gap = (4 * activity.resources.displayMetrics.density).toInt()
            val inner = kinds.width - kinds.paddingLeft - kinds.paddingRight
            if (inner <= 0) return@post
            val half = (inner - gap) / 2
            val tall = kinds.height - kinds.paddingTop - kinds.paddingBottom
            if (shelfThumb.layoutParams.width != half || shelfThumb.layoutParams.height != tall) {
                shelfThumb.layoutParams = shelfThumb.layoutParams.apply {
                    width = half
                    height = tall
                }
            }
            val target = if (kind == Library.Kind.VIDEO) 0f else (half + gap).toFloat()
            if (shelfThumb.translationX == target) return@post
            if (!shelfThumb.isLaidOut) {
                shelfThumb.translationX = target
            } else {
                shelfThumb.animate().translationX(target)
                    .setDuration(activity.resources.getInteger(R.integer.t_swap).toLong())
                    .setInterpolator(
                        android.view.animation.AnimationUtils.loadInterpolator(
                            activity, R.interpolator.ease
                        )
                    )
                    .start()
            }
        }
    }

    private fun styleTab(tab: View, icon: android.widget.ImageView, text: TextView, on: Boolean) {
        tab.isSelected = on
        val colour = if (on) android.graphics.Color.WHITE else activity.getColor(R.color.muted)
        text.setTextColor(colour)
        text.setTypeface(null, if (on) android.graphics.Typeface.BOLD else android.graphics.Typeface.NORMAL)
        icon.imageTintList = android.content.res.ColorStateList.valueOf(colour)
    }

    /** The empty note, worded and marked for the shelf, shown only when it has nothing. */
    private fun showEmpty() {
        val music = tab == Library.Kind.MUSIC
        // A filtered view that is empty is not an empty library, and saying
        // "nothing is saved" while a folder is in force is a lie the user can
        // see through — the shelf said there were files a moment ago.
        emptyText.setText(
            when {
                showing != null -> R.string.library_empty_folder
                music -> R.string.library_empty_music
                else -> R.string.library_empty
            }
        )
        emptyIcon.setImageResource(if (music) R.drawable.ic_music else R.drawable.ic_video)
        empty.visibility = if (shown().isEmpty()) View.VISIBLE else View.GONE
    }

    // ---- coming and going --------------------------------------------------

    /**
     * A sideways fling over the sheet turns the shelf: leftward to music,
     * rightward to video — the directions the switch itself lays them out in,
     * so the content appears to slide the way the finger sent it.
     *
     * Fed by the activity's dispatch rather than listened for here: the list
     * consumes its own touches for scrolling, and a listener on any one view
     * would go deaf the moment a child claimed the gesture. Observing the
     * dispatch stream costs nothing and misses nothing.
     *
     * Only over the sheet (the folder strip and the list): the stage already
     * answers horizontal gestures with seek and stop, and full screen is the
     * player's alone.
     */
    private val shelfSwipe = android.view.GestureDetector(
        activity,
        object : android.view.GestureDetector.SimpleOnGestureListener() {
            override fun onFling(
                from: android.view.MotionEvent?,
                to: android.view.MotionEvent,
                velocityX: Float,
                velocityY: Float,
            ): Boolean {
                val start = from ?: return false
                if (expanded) return false
                val origin = IntArray(2)
                chrome.getLocationOnScreen(origin)
                if (start.rawY < origin[1]) return false
                val across = to.rawX - start.rawX
                val down = to.rawY - start.rawY
                val far = SWIPE_DP * activity.resources.displayMetrics.density
                // Clearly sideways, or a scroll that drifts would turn the shelf.
                if (kotlin.math.abs(across) < far ||
                    kotlin.math.abs(across) < kotlin.math.abs(down) * 1.5f
                ) return false
                when {
                    across < 0 && tab == Library.Kind.VIDEO -> selectTab(Library.Kind.MUSIC)
                    across > 0 && tab == Library.Kind.MUSIC -> selectTab(Library.Kind.VIDEO)
                }
                return true
            }
        },
    )

    /** Every touch the activity dispatches while this screen is up. */
    fun observeTouch(event: android.view.MotionEvent) {
        if (isOpen) shelfSwipe.onTouchEvent(event)
    }

    /** Read the store again — used when a media permission has just arrived. */
    fun refresh() {
        if (isOpen) reload()
    }

    fun open() {
        onOpen?.invoke()
        root.visibility = View.VISIBLE
        root.bringToFront()
        // Asked for again on the way in: the window handed the bars out before
        // this screen existed, and a listener added afterwards hears nothing
        // until something asks. Without it the title sat under the clock.
        androidx.core.view.ViewCompat.requestApplyInsets(root)
        reload()
        watch()
        // The very first time the library opens, the earliest thumbnails decode a beat after the
        // list first lays out — some land on the ListView's throwaway measurement row and leave the
        // visible row showing its placeholder at the wrong size (user hit: "first entry thumbnails
        // don't fit"). A single re-bind once they have had a moment to decode fixes it, the same way
        // leaving and re-entering a folder did by hand. Once is enough — later opens are warm.
        if (!warmedThumbs) {
            warmedThumbs = true
            ui.postDelayed({ if (isOpen) adapter.notifyDataSetChanged() }, 500)
        }
        // Something left playing when the library was last closed is still going,
        // so its player comes back with the screen — but only onto the shelf it
        // belongs to.
        syncPlayerView()
    }

    /** First-open thumbnail warm-up runs once; see [open]. */
    private var warmedThumbs = false

    /**
     * Show the player that belongs to the shelf being looked at, and only that.
     *
     * Two things decide it together: what is playing, and which shelf is open.
     * They come apart — the sound outlives both a tab switch and the library
     * being closed — and reading only one of them is what put a song's bar over
     * the video shelf, left a video playing with no picture after a look at the
     * music, and left the last video's controls hanging over a song. One place
     * decides, and everywhere that can change either calls it.
     */
    private fun syncPlayerView() {
        val item = playing
        val onItsShelf = item != null && player != null && item.kind == tab

        if (onItsShelf && item!!.kind == Library.Kind.VIDEO) {
            stageArt.visibility = View.GONE
            surface.visibility = View.VISIBLE
            stage.visibility = View.VISIBLE
            stage.layoutParams = stage.layoutParams.apply { height = stageHeight() }
            // The surface went with the stage when it was hidden, so the player
            // is handed it again — otherwise the sound plays on over a picture
            // that never comes back.
            player?.setVideoSurfaceView(surface)
            stage.post { fitSurface() }
        } else if (onItsShelf && item!!.kind == Library.Kind.MUSIC) {
            // No picture on the whole screen for a song here — that is a desktop
            // nicety. The bar at the top carries the tile, and the list keeps its
            // covers as before; the stage just stays away.
            stageArt.visibility = View.GONE
            stage.visibility = View.GONE
        } else {
            stage.visibility = View.GONE
            // The scrub bar belongs to the picture; leaving it up over another
            // shelf is what made it linger and then vanish on its own.
            showControls(false)
        }

        if (onItsShelf && item!!.kind == Library.Kind.MUSIC) showNowPlaying(item) else hideNowPlaying()
    }


    /**
     * Keep looking, while the screen is open.
     *
     * The library reads storage rather than keeping its own copy, so something
     * removed by a file manager — or finished downloading — only shows up when
     * it is looked for. Looking once on opening meant a screen left open went
     * stale and stayed stale.
     */
    private fun watch() {
        ui.removeCallbacks(tick)
        ui.postDelayed(tick, REFRESH_MS)
    }

    private val tick = object : Runnable {
        override fun run() {
            if (!isOpen) return
            reload()
            ui.postDelayed(this, REFRESH_MS)
        }
    }

    /**
     * Leave, in the order a back press means it.
     *
     * Expanded is a state within the library, so the first back leaves that and
     * the second leaves the library. Returns false when there was nothing left
     * to close, which is how the activity knows the press was not for us.
     */
    fun back(): Boolean = when {
        !isOpen -> false
        expanded -> {
            setExpanded(false)
            true
        }
        else -> {
            close()
            true
        }
    }

    private var pullStartX = 0f
    private var pullStartY = 0f
    private var pulling = false

    /**
     * Follow the finger for a rightward drag on the windowed player and let it
     * commit past a third of the way — so leaving a video feels like pulling the
     * library aside to the web behind it, not a switch flipping.
     *
     * Returns true while a pull is active, so the tap/scroll/fling detector is
     * held off for the duration.
     */
    private fun handlePull(e: android.view.MotionEvent): Boolean {
        if (expanded) { pulling = false; return false }
        val density = activity.resources.displayMetrics.density
        when (e.actionMasked) {
            android.view.MotionEvent.ACTION_DOWN -> {
                pullStartX = e.rawX; pullStartY = e.rawY; pulling = false
            }
            android.view.MotionEvent.ACTION_MOVE -> {
                val dx = e.rawX - pullStartX
                val dy = e.rawY - pullStartY
                if (!pulling) {
                    // Begin only once it is clearly a rightward, horizontal drag,
                    // or a downward brightness/sound drag would never get a turn.
                    if (dx > 16 * density && dx > kotlin.math.abs(dy) * 1.5f) pulling = true
                    else return false
                }
                // A touch of resistance, so it drags rather than snaps.
                root.translationX = (e.rawX - pullStartX).coerceAtLeast(0f) * 0.9f
                return true
            }
            android.view.MotionEvent.ACTION_UP,
            android.view.MotionEvent.ACTION_CANCEL -> {
                if (!pulling) return false
                pulling = false
                // Commit at about a fifth of the way — roughly the reach a
                // shelf-switch swipe needs, so leaving to the web is no harder
                // than switching video/music.
                if (root.translationX > root.width * 0.2f) {
                    root.animate().translationX(root.width.toFloat()).setDuration(160)
                        .withEndAction { root.translationX = 0f; back() }.start()
                } else {
                    root.animate().translationX(0f).setDuration(160).start()
                }
                return true
            }
        }
        return pulling
    }

    /** Let go of the player for good; the screen is not coming back. */
    fun release() {
        released = true
        ui.removeCallbacks(tick)
        // Drop the process-global hooks so they stop pinning this screen alive —
        // but only the ones still ours, in case a later screen replaced them.
        if (EngineControl.stopPlayback === stopHook) EngineControl.stopPlayback = null
        if (EngineControl.togglePlayback === toggleHook) EngineControl.togglePlayback = null
        thumbWork.shutdown()
        thumbCache.evictAll()
        player?.release()
        player = null
        prepared = false
        clearMediaControls()
    }

    /**
     * Leave the library.
     *
     * With background playback on, the video keeps going and the notification
     * keeps its controls — back returns to the browser and the sound carries on.
     * With it off, leaving the library is leaving playback: being anywhere but
     * here is the "background" the setting turns off, so the video stops.
     */
    fun close() {
        ui.removeCallbacks(tick)
        setExpanded(false)
        root.visibility = View.GONE
        if (!Library.backgroundPlayback(activity)) stopPlaying()
        onClose?.invoke()
    }

    // ---- the list ----------------------------------------------------------

    private fun reload() {
        work.execute {
            val found = runCatching { Library.items(activity) }.getOrDefault(emptyList())
            val names = runCatching { Library.folders(activity, found, tab) }.getOrDefault(emptyList())
            ui.post {
                // Nothing is touched when nothing moved. Redrawing a list on a
                // timer would fight whoever is scrolling it.
                val same = found.map { Triple(it.id, it.folder, it.kind) } ==
                    items.map { Triple(it.id, it.folder, it.kind) } && names == folders
                if (same) return@post
                items = found
                folders = names
                // A folder that is no longer there stops being the one shown,
                // or the list would be empty with no way to say why.
                if (showing != null && showing !in names) showing = null
                drawFolders()
                adapter.notifyDataSetChanged()
                showEmpty()
            }
        }
    }

    /** Where a run of double taps is seeking to, or -1 between runs. */
    private var seekTarget = -1
    private val forgetSeek = Runnable { seekTarget = -1 }

    /**
     * Jump [delta] milliseconds, adding up across a run of quick taps.
     *
     * A seek is not instant, and the position does not move until it lands — so
     * a second tap that read `currentPosition` again would compute from the spot
     * the first tap had not yet reached, and several fast taps would all aim at
     * roughly the same place. Adding onto a running target instead makes each tap
     * count. The target is forgotten once the tapping stops, so the next run
     * starts from wherever the video actually is.
     */
    private fun seekBy(delta: Int) {
        val mp = player ?: return
        if (!prepared) return
        val duration = mp.duration.toInt()
        val from = if (seekTarget >= 0) seekTarget else mp.currentPosition.toInt()
        val target = if (duration > 0) (from + delta).coerceIn(0, duration) else (from + delta).coerceAtLeast(0)
        seekTarget = target
        // Exactly there, not the nearest keyframe: ExoPlayer seeks to the frame,
        // so a 15s video double-tapped forward lands on 16, not back on a sync
        // frame at 12 — three seconds means three.
        mp.seekTo(target.toLong())
        ui.removeCallbacks(forgetSeek)
        ui.postDelayed(forgetSeek, SEEK_SETTLE_MS)
        showControls(true)
        refreshMediaNotification()
    }

    /**
     * What the list is showing: the current shelf, narrowed to a folder.
     *
     * The shelf comes first — videos or music — and the folder filter only
     * applies within videos, since music is kept as one flat list.
     */
    private fun shown(): List<Library.Item> {
        val shelf = Library.arranged(activity, items.filter { it.kind == tab }, tab)
        // Each folder is its own list, and so is the top level ("저장소"): a
        // folder is a playlist, not a filter over everything. So the top chip
        // shows only what sits at the top, not every file gathered from every
        // folder.
        val name = showing ?: ""
        return shelf.filter { it.folder == name }
    }

    /** Size, folder and age — the three things worth knowing about a kept file. */
    private fun describe(item: Library.Item): String {
        val parts = mutableListOf(Downloads.bytes(item.bytes))
        if (item.folder.isNotBlank()) parts += item.folder
        parts += age(item.addedAt)
        return parts.joinToString("  ·  ")
    }

    private fun age(seconds: Long): String {
        if (seconds <= 0) return ""
        val days = (System.currentTimeMillis() / 1000 - seconds) / 86_400
        return when {
            days <= 0 -> "오늘"
            days == 1L -> "어제"
            days < 30 -> "${days}일 전"
            else -> "${days / 30}개월 전"
        }
    }

    /**
     * What a swipe over the picture means.
     *
     * Up fills the screen, down gives the list back, left puts the video away.
     * A tap brings the controls up — including a tap on the black beside the
     * picture, which is part of the player as far as anyone watching is
     * concerned even though nothing is drawn there.
     */
    private val gestures = android.view.GestureDetector(
        activity,
        object : android.view.GestureDetector.SimpleOnGestureListener() {
            override fun onDown(e: android.view.MotionEvent) = true

            /**
             * A held press runs the picture fast on the right, rewinds on the
             * left. The middle is left alone — nothing there to hold. Ended by
             * the touch listener when the finger lifts.
             */
            override fun onLongPress(e: android.view.MotionEvent) {
                startHold(e.x)
            }

            /**
             * A tap shows the bar, and a tap while it is up puts it away.
             *
             * It used to play or pause on the second tap. That is what the button
             * and the spacebar are for; a whole picture that toggles playback
             * fires on the way into every gesture and cannot be used just to see
             * the controls. So a tap is only ever about the bar now — bring it
             * up, or dismiss it. Confirmed rather than raw, so the first tap of a
             * double tap does not flash the controls on its way to a seek.
             */
            override fun onSingleTapConfirmed(e: android.view.MotionEvent): Boolean {
                if (controlsShown) showControls(false) else showControls(true)
                return true
            }

            /**
             * A double tap seeks: forward on the right of the picture, back on
             * the left, three seconds a time — the gesture every video app uses.
             *
             * The middle is left alone so a double tap there cannot be read as
             * either direction; it just toggles the controls like a single tap.
             */
            override fun onDoubleTap(e: android.view.MotionEvent): Boolean {
                if (player == null || !prepared) return false
                val third = stage.width / 3f
                val delta = when {
                    e.x > third * 2 -> SEEK_MS
                    e.x < third -> -SEEK_MS
                    else -> return false
                }
                seekBy(delta)
                return true
            }

            /**
             * A slow drag down the sides, while full screen.
             *
             * The left of the picture is the screen's brightness and the right
             * is the sound, which is where every video app puts them. Only the
             * sides: the middle is left for the swipes that come and go from
             * full screen, so one gesture never has to guess at two meanings.
             */
            override fun onScroll(
                from: android.view.MotionEvent?,
                to: android.view.MotionEvent,
                acrossBy: Float,
                downBy: Float,
            ): Boolean {
                if (!expanded) return false
                val start = from ?: return false
                if (kotlin.math.abs(downBy) < kotlin.math.abs(acrossBy)) return false
                // A full sweep of the stage is the whole range, so the reach
                // needed does not depend on how big the phone is.
                val step = downBy / stage.height.coerceAtLeast(1)
                return when (side(start.x)) {
                    Side.LEFT -> {
                        changeBrightness(step)
                        true
                    }
                    Side.RIGHT -> {
                        changeVolume(step)
                        true
                    }
                    Side.MIDDLE -> false
                }
            }

            override fun onFling(
                from: android.view.MotionEvent?,
                to: android.view.MotionEvent,
                velocityX: Float,
                velocityY: Float,
            ): Boolean {
                val start = from ?: return false
                // While full screen the sides belong to brightness and sound; a
                // flick down them is not a request to leave.
                if (expanded && side(start.x) != Side.MIDDLE) return false
                val across = to.x - start.x
                val down = to.y - start.y
                val far = SWIPE_DP * activity.resources.displayMetrics.density
                return when {
                    // Whichever direction went further decides, so a swipe that
                    // wanders does what it mostly did.
                    kotlin.math.abs(down) > kotlin.math.abs(across) && down < -far -> {
                        setExpanded(true)
                        true
                    }
                    kotlin.math.abs(down) > kotlin.math.abs(across) && down > far -> {
                        setExpanded(false)
                        true
                    }
                    across < -far -> {
                        stopPlaying()
                        true
                    }
                    // Rightward-to-web is handled as an interactive pull (see the
                    // stage touch listener), so it can follow the finger — not
                    // caught here as an instant fling.
                    else -> false
                }
            }
        },
    )

    /**
     * How much of the stage the picture fills, while full screen.
     *
     * A video letterboxed to fit leaves black above and below it, and on a
     * phone held sideways that is most of the screen. Pinching trades the edges
     * of the picture for the height of it, the way every video app does.
     */
    private var zoom = 1f

    private val pinch = android.view.ScaleGestureDetector(
        activity,
        object : android.view.ScaleGestureDetector.SimpleOnScaleGestureListener() {
            override fun onScale(detector: android.view.ScaleGestureDetector): Boolean {
                if (!expanded) return false
                // Bounded: past a point the picture is a detail of itself, and
                // getting back to the whole of it becomes the hard part.
                zoom = (zoom * detector.scaleFactor).coerceIn(1f, 3f)
                fitSurface()
                return true
            }
        },
    )

    private enum class Side { LEFT, MIDDLE, RIGHT }

    /** Which third of the picture a touch landed in. */
    private fun side(x: Float): Side {
        val third = stage.width / 3f
        return when {
            x < third -> Side.LEFT
            x > third * 2 -> Side.RIGHT
            else -> Side.MIDDLE
        }
    }

    /**
     * Louder or quieter, by however much the finger moved.
     *
     * The system's own indicator is asked for rather than one being drawn: it
     * is the one the phone shows for its buttons, so the two gestures for the
     * same thing look like the same thing.
     */
    private fun changeVolume(step: Float) {
        val audio = activity.getSystemService(android.media.AudioManager::class.java) ?: return
        val most = audio.getStreamMaxVolume(android.media.AudioManager.STREAM_MUSIC)
        volumeAt += step * most
        val whole = volumeAt.toInt()
        // The gauge follows even a step too small to move the volume a whole
        // notch, so it shows the moment the finger does, not a notch later.
        val current = audio.getStreamVolume(android.media.AudioManager.STREAM_MUSIC)
        if (whole != 0) {
            volumeAt -= whole
            val now = (current + whole).coerceIn(0, most)
            // The app's own gauge, not the system's — one look for both this and
            // brightness, and shown the instant the drag begins.
            audio.setStreamVolume(android.media.AudioManager.STREAM_MUSIC, now, 0)
            showGauge(R.drawable.ic_volume, now.toFloat() / most)
            // The bar's slider is the same level; keep it under the drag.
            if (controlsShown) syncVolumeBar()
        } else {
            showGauge(R.drawable.ic_volume, current.toFloat() / most)
        }
    }

    /**
     * Brighter or darker, for this window only.
     *
     * The phone's own setting is left alone: a video app that dims the screen
     * and keeps it dimmed after it is closed is a fault report waiting to
     * happen. Leaving full screen puts it back to whatever the phone decides.
     */
    private fun changeBrightness(step: Float) {
        val window = activity.window
        val attributes = window.attributes
        val now = attributes.screenBrightness.takeIf { it >= 0f } ?: 0.5f
        // Up is brighter: a swipe up the left of the picture raises the light,
        // the way the other side raises the sound.
        val next = (now + step).coerceIn(0.02f, 1f)
        attributes.screenBrightness = next
        window.attributes = attributes
        showGauge(R.drawable.ic_brightness, next)
    }

    /** Part of a volume step carried over between drags. */
    private var volumeAt = 0f

    private val hideGauge = Runnable { gauge.visibility = View.GONE }

    /** Flash the gauge at [level] (0..1) with its mark, then let it fade after a beat. */
    private fun showGauge(icon: Int, level: Float) {
        gaugeIcon.setImageResource(icon)
        gaugeBar.progress = (level.coerceIn(0f, 1f) * 100).toInt()
        gauge.visibility = View.VISIBLE
        gauge.bringToFront()
        ui.removeCallbacks(hideGauge)
        ui.postDelayed(hideGauge, 800)
    }

    // ---- playing -----------------------------------------------------------

    /**
     * The player, held by this screen rather than by a view.
     *
     * A `VideoView` releases its player when its surface goes, and a surface
     * goes as soon as the app leaves the screen — so sound could never carry on
     * behind another app while one was in charge. Owning the player here means
     * the picture can go and the sound stay.
     */
    private var player: ExoPlayer? = null

    // ---- press-and-hold on the picture: right runs fast, left rewinds --------
    //
    // The same gesture the desktop player has, and the one every long-form phone
    // player has settled on: hold the right of the picture and it runs at 2×,
    // hold the left and it rewinds continuously, let go and it is as it was. The
    // hold is a long-press so a still finger starts it and a drag does not — a
    // drag down the sides already means brightness and sound.

    /** The speed to return to when a right-hold ends; null when not held. */
    private var heldSpeed: Float? = null

    /** Running while a left-hold rewinds; null when not. */
    private var rewinding = false

    /** Whether the film was playing when a left-rewind began. */
    private var rewindWasPlaying = false

    private val rewindStep = object : Runnable {
        override fun run() {
            val mp = player ?: return
            val to = (mp.currentPosition - 200L).coerceAtLeast(0L)
            mp.seekTo(to)
            npElapsedFromHold(to)
            if (to <= 0L) endHold() else ui.postDelayed(this, 50L)
        }
    }

    private fun npElapsedFromHold(at: Long) {
        // Keep the bar's reading honest while the hold moves the position under
        // it; the periodic tick is paused for a paused player.
        seekTarget = at.toInt()
    }

    private fun startHold(x: Float) {
        val mp = player ?: return
        if (!prepared) return
        val third = stage.width / 3f
        when {
            x > third * 2 -> {
                if (heldSpeed != null) return
                heldSpeed = mp.playbackParameters.speed
                mp.setPlaybackSpeed(2f)
                Toast.makeText(activity, "2× 재생", Toast.LENGTH_SHORT).show()
            }
            x < third -> {
                if (rewinding) return
                rewinding = true
                rewindWasPlaying = wantsPlay
                mp.pause()
                Toast.makeText(activity, "◀◀ 되감기", Toast.LENGTH_SHORT).show()
                ui.post(rewindStep)
            }
        }
    }

    private fun endHold() {
        val mp = player
        heldSpeed?.let {
            mp?.setPlaybackSpeed(it)
            heldSpeed = null
        }
        if (rewinding) {
            rewinding = false
            ui.removeCallbacks(rewindStep)
            // Left as it was found: playing on if it was playing, still if not —
            // a rewind is not a request to start playback nobody asked to resume.
            if (rewindWasPlaying) mp?.play()
        }
    }

    /**
     * Whether the player has finished preparing.
     *
     * Set when ExoPlayer first reaches the ready state, so the seek gesture and
     * the buttons know there is a duration to seek within and a frame to show.
     */
    private var prepared = false

    /**
     * Whether the video is meant to be playing, as opposed to what the player
     * momentarily reports.
     *
     * `MediaPlayer.isPlaying` can read false for an instant just after start, or
     * while a seek settles, and the notification drawn from it in that instant
     * showed a play button over a playing video. This is set the moment play or
     * pause is decided, so the button always matches the intent.
     */
    private var wantsPlay = false

    private fun play(item: Library.Item) {
        playing = item
        prepared = false
        wantsPlay = true
        // A new video: any pending seek run belongs to the last one.
        seekTarget = -1
        ui.removeCallbacks(forgetSeek)
        // A song has no picture of its own to decode. It plays in the bar at the
        // top under its cover, and the surface and the frame fitting are left
        // out of it, so the last video's frame never shows behind a song.
        npFor = null

        player?.release()

        // A fresh player per item, bound to the field before it is prepared so a
        // late notice — a ready or an end queued for a player since replaced,
        // which the async stop path can leave behind — can be checked against the
        // one in charge and ignored, rather than resurrecting the stage or
        // advancing after an explicit stop.
        val exo = ExoPlayer.Builder(activity).build()
        // Land on the exact frame a seek asks for, not the nearest keyframe.
        exo.setSeekParameters(SeekParameters.EXACT)
        player = exo

        // After the player is in the field, not before it. `syncPlayerView`
        // asks whether there is a player at all to decide whether there is
        // anything to show, so asking on the way in — while the field still
        // held the last one, or nothing — left the first video of a session
        // playing its sound with the stage still hidden. Nothing called this
        // again until a shelf was switched or something else was played, so the
        // picture only turned up on the second attempt.
        syncPlayerView()

        exo.addListener(object : Player.Listener {
            override fun onPlaybackStateChanged(state: Int) {
                if (player !== exo) return
                when (state) {
                    Player.STATE_READY -> if (!prepared) onPlayerReady(item)
                    Player.STATE_ENDED -> onPlaybackEnded()
                }
            }

            override fun onIsPlayingChanged(isPlaying: Boolean) {
                if (player !== exo) return
                // Seeking back into a finished video restarts playback without
                // going through the toggle, so wantsPlay (the intent flag the
                // button reads) is left at false and the button wrongly shows
                // "play" while the video runs. When playback actually resumes,
                // make the intent agree. Only the resume direction is synced —
                // pausing still goes through the toggle, keeping its no-flicker
                // intent semantics.
                if (isPlaying && !wantsPlay) {
                    wantsPlay = true
                    updateVideoBar()
                    updateNowPlaying()
                    refreshMediaNotification()
                }
            }

            override fun onPlayerError(error: PlaybackException) {
                if (player !== exo) return
                // A named code, not a bare number: it says whether the file is
                // unreadable, the network dropped, or the phone has no decoder.
                toast(activity.getString(R.string.library_cannot_play) + " (${error.errorCodeName})")
                stopPlaying()
            }

            override fun onVideoSizeChanged(size: VideoSize) {
                if (player !== exo || item.kind != Library.Kind.VIDEO) return
                // The frame size is known now, so the picture can be letterboxed
                // and full screen turned the right way — including after autoplay
                // has moved from a landscape one to a short.
                fitSurface()
                applyOrientation()
            }
        })

        if (item.kind == Library.Kind.VIDEO) {
            exo.setVideoSurfaceView(surface)
        }
        exo.setMediaItem(MediaItem.fromUri(item.uri))
        // From the beginning, unless a place was put down on this file by hand.
        val hold = Library.holdAt(activity, item)
        if (hold > 1_000) exo.seekTo(hold)
        exo.playWhenReady = true
        exo.prepare()
    }

    /** The player has a duration and a first frame: settle the surfaces around it. */
    private fun onPlayerReady(item: Library.Item) {
        prepared = true
        wantsPlay = true
        if (item.kind == Library.Kind.MUSIC) {
            updateNowPlaying()
        } else {
            showControls(true)
            fitSurface()
            applyOrientation()
        }
        startMediaControls(item)
    }

    /** The end: go on to the next, or mark it over so the button reads "play". */
    private fun onPlaybackEnded() {
        if (Library.playbackEnd(activity) != Library.PlaybackEnd.STOP) {
            advance()
        } else {
            // Over, not paused. Marking it so turns the notification's button
            // from a pause back into a play — a finished video showing a pause,
            // and restarting when pressed, is the wrong thing on the one surface
            // left while the app is in the background. Pressing play replays it.
            wantsPlay = false
            showControls(true)
            refreshMediaNotification()
        }
    }

    /**
     * Play whatever comes after the one that just finished.
     *
     * The list is taken fresh — it is re-read on a timer and may have changed
     * under the player — and the current item is found by id rather than by
     * position, so a video removed or a folder re-sorted in the meantime does
     * not hand this the wrong one.
     *
     * The queue is the shelf the finished item belongs to — its own kind, and
     * for a video the folder it sits in — not whatever tab happens to be
     * showing. They come apart: the sound outlives a tab switch, so keying off
     * the visible tab would let a finished song shuffle into a video, or a
     * "keep playing" queue stop dead because the played item is not on the shelf
     * being looked at. Keeping to the item's own shelf holds both to sense.
     */
    private fun advance() {
        val current = playing ?: return
        val list = queueFor(current)
        val currentId = current.id

        val next = if (Library.playbackEnd(activity) == Library.PlaybackEnd.SHUFFLE) {
            // A different one at random. Filtered so a folder of one does not
            // replay the same video for ever; with nothing else, it stops.
            val others = list.filter { it.id != currentId }
            others.randomOrNull()
        } else {
            // The next one down the list. `null` at the end, which stops rather
            // than looping — a phone in a pocket should not play the whole
            // library twice over.
            val at = list.indexOfFirst { it.id == currentId }
            list.getOrNull(at + 1)?.takeIf { at >= 0 }
        }

        if (next == null) {
            // Nothing to go to: leave the last frame up with the controls, the
            // same as a video ending with autoplay off.
            showControls(true)
            refreshMediaNotification()
            return
        }
        play(next)
    }

    /**
     * The shelf a playing item carries on within: its own kind, and for a video
     * the folder it is in. Music is one flat list. Independent of the visible
     * tab, so autoplay never crosses from songs to videos or back.
     */
    private fun queueFor(item: Library.Item): List<Library.Item> {
        val shelf = items.filter { it.kind == item.kind }
        return if (item.kind == Library.Kind.VIDEO && item.folder.isNotBlank()) {
            shelf.filter { it.folder == item.folder }
        } else {
            shelf
        }
    }

    /** Letterbox the picture inside the stage rather than stretching it. */
    private fun fitSurface() {
        val size = player?.videoSize ?: return
        val videoWidth = size.width
        val videoHeight = size.height
        if (videoWidth <= 0 || videoHeight <= 0) return
        val room = stage.width.takeIf { it > 0 } ?: return
        val tall = stage.height.takeIf { it > 0 } ?: return
        val scale = minOf(room.toFloat() / videoWidth, tall.toFloat() / videoHeight) * zoom
        surface.layoutParams = (surface.layoutParams as FrameLayout.LayoutParams).apply {
            width = (videoWidth * scale).toInt()
            height = (videoHeight * scale).toInt()
            gravity = android.view.Gravity.CENTER
        }
    }

    private fun stopPlaying() {
        // Nothing drives the screen once it is let go — a stop posted just
        // before release must not touch the torn-down activity.
        if (released) return
        seekTarget = -1
        ui.removeCallbacks(forgetSeek)
        player?.release()
        player = null
        prepared = false
        wantsPlay = false
        showControls(false)
        playing = null
        expanded = false
        activity.requestedOrientation = android.content.pm.ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        chrome.visibility = View.VISIBLE
        header.visibility = View.VISIBLE
        stage.visibility = View.GONE
        stageArt.setImageDrawable(null)
        npFor = null
        hideNowPlaying()
        clearMediaControls()
    }

    /**
     * Play or pause from outside the picture — the notification, the lock
     * screen, a headset button. All of them arrive here.
     */
    private fun togglePlayPause() {
        val mp = player ?: return
        if (!prepared) return
        // Toggle against the intent the button is showing, not the player's
        // instant state: the notification is drawn from `wantsPlay`, so deciding
        // from it too keeps the button and what it does the same.
        if (wantsPlay) {
            mp.pause()
            wantsPlay = false
        } else {
            mp.play()
            wantsPlay = true
        }
        refreshMediaNotification()
    }

    /**
     * The app is going to the background.
     *
     * With background playback on, the video carries on and the notification
     * keeps its controls. With it off, the library not being on screen means the
     * video is not playing at all — being anywhere but the library is the
     * "background" this setting is about — so it stops, and its controls leave
     * the notification with it.
     */
    fun pause() {
        if (player == null) return
        if (!Library.backgroundPlayback(activity)) stopPlaying()
    }

    /**
     * Back in the foreground.
     *
     * ExoPlayer reattaches to the surface view and repaints the stopped frame on
     * its own, so there is nothing to hand back — only the notification to bring
     * back into step. Nothing is started that was not already playing.
     */
    fun resume() {
        if (player == null) return
        refreshMediaNotification()
    }

    // ---- controls in the notification shade --------------------------------
    //
    // The video shares the engine switch's notification rather than posting its
    // own. What is playing is told to the process-wide bridge; the service that
    // draws the one notification reads it there and puts a play/pause and a stop
    // button beside the switch. No second notification, no media session — the
    // buttons are the app's own, wired to the same two methods the shade's
    // taps reach.

    /** Announce a freshly started video to the notification. */
    private fun startMediaControls(item: Library.Item) {
        EngineControl.playbackTitle = item.title
        refreshMediaNotification()
    }

    /** Tell the notification the current playing/paused state, or that it is
     *  over. Kept under the old name because it is called from many places. */
    private fun refreshMediaNotification() {
        if (player == null || playing == null) {
            clearMediaControls()
            return
        }
        EngineControl.playbackTitle = playing?.title
        // The intent, not the instant: isPlaying can read false for a moment
        // right after start or during a seek, and the button must not flicker.
        EngineControl.playbackPlaying = prepared && wantsPlay
        EngineControl.onPlaybackChanged?.invoke()
        // The music bar is the same state on another face; keep it in step.
        updateNowPlaying()
    }

    /** Take the controls out of the notification. */
    private fun clearMediaControls() {
        EngineControl.playbackTitle = null
        EngineControl.playbackPlaying = false
        EngineControl.onPlaybackChanged?.invoke()
    }

    /**
     * Sixteen by nine, which is the shape almost every video is, and never more
     * than half the screen — a player taller than that leaves the list a strip.
     */
    private fun stageHeight(): Int {
        if (expanded) return ViewGroup.LayoutParams.MATCH_PARENT
        val metrics = activity.resources.displayMetrics
        // A song's bar is three rows deep — what is playing, the line, the
        // transport — where a video's sits over the picture and costs nothing.
        // Half the screen plus that bar is more than a phone held sideways has,
        // and the transport went off the bottom edge with no way to reach it.
        // Only music is held back; a video keeps the half it always had.
        val cap =
            if (playing?.kind == Library.Kind.MUSIC) metrics.heightPixels / 3
            else metrics.heightPixels / 2
        return minOf(metrics.widthPixels * 9 / 16, cap)
    }

    /**
     * Full screen, and turned on its side.
     *
     * A video is wider than it is tall and a phone is not, so filling the screen
     * without turning it fills a third of it — but only a video that is itself
     * wider than tall. A portrait video (a short) already matches the phone
     * held upright, and turning the phone sideways for it would waste the screen
     * exactly the way turning it for a landscape video saves it. So the turn
     * follows the shape of what is playing.
     */
    private fun setExpanded(on: Boolean) {
        // Only a video has a picture to fill the screen. Music stays as it is —
        // this catches the swipe-up as well as the (hidden) button, so a flick on
        // the note cannot drop into a black full screen with nothing in it.
        expanded = on && playing?.kind == Library.Kind.VIDEO
        // The header goes with the list. It sits above the picture now, so full
        // screen has to put both away or the picture would start a bar's height
        // down the screen.
        chrome.visibility = if (expanded) View.GONE else View.VISIBLE
        header.visibility = if (expanded) View.GONE else View.VISIBLE
        // The system's own bars, too: full screen means the whole screen, not a
        // picture with the clock and the gesture pill still sitting on it. They
        // come back the moment it is left, and a swipe from the edge brings them
        // up briefly the way every video app allows.
        setSystemBars(hidden = expanded)
        stage.layoutParams = stage.layoutParams.apply { height = stageHeight() }
        if (!expanded) {
            zoom = 1f
            // The screen goes back to the phone's own brightness.
            activity.window.attributes = activity.window.attributes.apply {
                screenBrightness = android.view.WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
            }
        }
        applyOrientation()
        expand.setImageResource(
            if (expanded) R.drawable.ic_collapse else R.drawable.ic_expand
        )
        expand.contentDescription = activity.getString(
            if (expanded) R.string.library_collapse else R.string.library_expand
        )
        // The way back, in the corner every screen keeps it. Full screen hides
        // the header this normally lives in, and a gesture nobody was told
        // about is not a way out. It travels with the bar, so whether it is up
        // is the bar's question — this only says whether it is allowed at all.
        fade(leaveFullScreen, expanded && controlsShown)
        // The title clears the back button in full screen: the button sits in
        // the top-left corner then, and without this the name ran under it.
        val density = activity.resources.displayMetrics.density
        val start = ((if (expanded) 56 else 14) * density).toInt()
        stageTitle.setPaddingRelative(
            start,
            (10 * density).toInt(),
            (14 * density).toInt(),
            (18 * density).toInt(),
        )
        stage.post { fitSurface() }
    }

    /**
     * Hide or restore the status and navigation bars.
     *
     * `BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE` is what lets a swipe from the edge
     * peek them back without leaving full screen — the behaviour every video
     * player has, and the one a locked-away bar needs so the way out is never
     * truly gone.
     */
    private fun setSystemBars(hidden: Boolean) {
        val controller = androidx.core.view.WindowCompat.getInsetsController(
            activity.window,
            activity.window.decorView,
        )
        controller.systemBarsBehavior =
            androidx.core.view.WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        val bars = androidx.core.view.WindowInsetsCompat.Type.systemBars()
        if (hidden) controller.hide(bars) else controller.show(bars)
    }

    /**
     * Turn the phone, or don't, to match the video that is full screen.
     *
     * A landscape video turns the screen sideways so it fills it; a portrait one
     * is left upright for the same reason. Read from the player's own frame
     * size, which is only known once it is prepared — before that, a landscape
     * guess, since most videos are. Re-applied when a new video is prepared
     * while already full screen, so autoplay onto a short does not leave the
     * phone stuck sideways.
     */
    private fun applyOrientation() {
        val width = player?.videoSize?.width ?: 0
        val height = player?.videoSize?.height ?: 0
        activity.requestedOrientation = when {
            !expanded -> android.content.pm.ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            // Size not known yet: do not lock to anything. Expanding before the
            // player is prepared would otherwise guess landscape and turn a
            // short sideways for a moment; onPrepared runs this again with the
            // real dimensions and locks it correctly then.
            width <= 0 || height <= 0 ->
                android.content.pm.ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            width < height -> android.content.pm.ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
            else -> android.content.pm.ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        }
    }

    // ---- what can be done to one --------------------------------------------

    private fun showMenu(anchor: View, item: Library.Item) {
        // Two things, and both are about the file itself.
        //
        // Filing it somewhere is done by carrying it there — the gesture says
        // where it is going, which a list of folder names cannot — and where it
        // should start from is set on the picture while it plays. What is left
        // here is what has nowhere else to be.
        val menu = PopupMenu(activity, anchor)
        menu.menu.add(0, RENAME, 0, R.string.library_rename)
        menu.menu.add(0, DELETE, 1, R.string.library_delete)
        menu.setOnMenuItemClickListener { chosen ->
            when (chosen.itemId) {
                RENAME -> askForName(item)
                DELETE -> confirmDelete(item)
            }
            true
        }
        menu.show()
    }

    private fun moveTo(item: Library.Item, folder: String) {
        work.execute {
            val done = Library.move(activity, item, folder)
            ui.post {
                if (!done) toast(activity.getString(R.string.library_move_failed))
                reload()
            }
        }
    }

    private fun confirmDelete(item: Library.Item) {
        MaterialAlertDialogBuilder(activity)
            .setMessage(activity.getString(R.string.library_delete_ask, item.title))
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.library_delete) { _, _ ->
                work.execute {
                    val done = Library.delete(activity, item)
                    ui.post {
                        toast(
                            activity.getString(
                                if (done) R.string.library_deleted else R.string.library_delete_failed
                            )
                        )
                        // The one on the stage may be the one just removed.
                        if (done && playing?.id == item.id) stopPlaying()
                        reload()
                    }
                }
            }
            .show()
    }

    // ---- folders ------------------------------------------------------------

    private fun drawFolders() {
        folderRow.removeAllViews()
        val density = activity.resources.displayMetrics.density
        // The top-level chip is home — a house icon, not the word, so it reads as
        // "the whole storage" at a glance.
        val home = chip(activity.getString(R.string.library_all), showing == null, "") {
            showing = null
            drawFolders()
            adapter.notifyDataSetChanged()
            showEmpty()
        }
        home.text = ""
        home.setCompoundDrawablesRelativeWithIntrinsicBounds(R.drawable.ic_home, 0, 0, 0)
        home.compoundDrawablePadding = 0
        // Icon-only: hug the house. The shared chip() forces a 64dp floor and 16dp side
        // padding so word folders read as tabs, but on the imageless home chip that left a
        // small icon marooned in a wide tab. Drop the floor and tighten the sides to the
        // icon so the tab is the size of its picture.
        home.minimumWidth = 0
        val homePad = (10 * density).toInt()
        home.setPadding(homePad, home.paddingTop, homePad, home.paddingBottom)
        folderRow.addView(home)
        folders.forEach { name ->
            val chip = chip(name, showing == name, name) {
                showing = if (showing == name) null else name
                drawFolders()
                adapter.notifyDataSetChanged()
                showEmpty()
            }
            chip.setOnLongClickListener {
                // The same grammar a file row answers with: what it can do,
                // then the destructive thing last.
                MaterialAlertDialogBuilder(activity)
                    .setTitle(name)
                    .setItems(
                        arrayOf(
                            activity.getString(R.string.library_rename),
                            activity.getString(R.string.library_folder_remove),
                        ),
                    ) { _, which ->
                        if (which == 0) askToRenameFolder(name) else confirmRemoveFolder(name)
                    }
                    .show()
                true
            }
            folderRow.addView(chip)
        }
        // The way to make a folder: the same button the desktop's tab strip
        // ends with — a small rounded square beside the tabs, not a tab among
        // them. A + drawn as a tab promised a folder that does not exist yet;
        // a button beside the row says "this makes more of these".
        val add = TextView(activity)
        add.text = "+"
        add.textSize = 15f
        add.setTextColor(activity.getColor(R.color.muted))
        add.gravity = android.view.Gravity.CENTER
        add.includeFontPadding = false
        add.setBackgroundResource(R.drawable.new_tab)
        // A shade smaller than the tabs (26 against their ~36), so the row
        // reads as tabs-plus-a-button rather than as one more tab shape.
        add.layoutParams = LinearLayout.LayoutParams(
            (26 * density).toInt(),
            (26 * density).toInt(),
        ).apply {
            gravity = android.view.Gravity.BOTTOM
            marginStart = (6 * density).toInt()
            // Low against the tabs rather than centred on them: the tabs'
            // labels sit in the lower half of their shape, and a + centred on
            // the SHAPE floated above the text it stands beside.
            bottomMargin = (2 * density).toInt()
        }
        add.setOnClickListener { askForFolder() }
        folderRow.addView(add)
    }

    /**
     * One folder button.
     *
     * [target] is the folder a row dropped here belongs in — the name itself,
     * or "" for the chip that means everything, which is also how something is
     * taken back out of a folder.
     */
    private fun chip(label: String, on: Boolean, target: String, tap: () -> Unit): TextView {
        val view = TextView(activity)
        val density = activity.resources.displayMetrics.density
        view.text = label
        view.isSelected = on
        // The same chip the desktop draws: outlined while idle, filled while in
        // force. A folder is a filter over the list, not a place navigated to,
        // and a chip is what a filter looks like. The underlined-tab drawing
        // this used to be gave the same feature two shapes across the two apps.
        view.setBackgroundResource(R.drawable.folder_tab)
        // The hair between tabs, as a browser draws them: near, not touching.
        view.layoutParams = LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        ).apply { marginEnd = (3 * density).toInt() }
        view.setTextColor(activity.getColor(if (on) R.color.on_surface else R.color.muted))
        view.textSize = 13f
        view.setTypeface(
            view.typeface,
            if (on) android.graphics.Typeface.BOLD else android.graphics.Typeface.NORMAL,
        )
        val padH = (16 * density).toInt()
        val padV = (6 * density).toInt()
        view.setPadding(padH, padV, padH, padV)
        // A floor under the width, so one-word folders read as tabs rather
        // than as text with corners. Centred, or a short name leans left in
        // the room the floor grants it.
        view.minimumWidth = (64 * density).toInt()
        view.gravity = android.view.Gravity.CENTER
        view.setOnClickListener { tap() }
        // A folder is somewhere to put things, so it accepts things being put
        // on it. The chip lifts while a row is over it, or a drop is a guess.
        view.setOnDragListener { chip, event ->
            when (event.action) {
                android.view.DragEvent.ACTION_DRAG_STARTED -> event.localState is Library.Item
                android.view.DragEvent.ACTION_DRAG_ENTERED -> {
                    chip.alpha = 0.6f
                    true
                }
                android.view.DragEvent.ACTION_DRAG_EXITED,
                android.view.DragEvent.ACTION_DRAG_ENDED -> {
                    chip.alpha = 1f
                    true
                }
                android.view.DragEvent.ACTION_DROP -> {
                    chip.alpha = 1f
                    (event.localState as? Library.Item)?.let { moveTo(it, target) }
                    true
                }
                else -> true
            }
        }
        val params = LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.WRAP_CONTENT,
            LinearLayout.LayoutParams.WRAP_CONTENT,
        )
        params.marginEnd = (4 * density).toInt()
        view.layoutParams = params
        return view
    }

    /**
     * Ask for a name, and put [moving] in it once there is one.
     *
     * Making a folder and filling it are the same act often enough that doing
     * them separately would be two steps for one intention.
     */
    private fun askForFolder(moving: Library.Item? = null) {
        val field = EditText(activity)
        field.hint = activity.getString(R.string.library_folder_name)
        field.setSingleLine()
        MaterialAlertDialogBuilder(activity)
            .setTitle(R.string.library_new_folder)
            .setView(field)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.confirm) { _, _ ->
                val name = Library.clean(field.text.toString())
                if (name.isBlank()) return@setPositiveButton
                work.execute {
                    Library.addFolder(activity, name, tab)
                    val done = moving?.let { Library.move(activity, it, name) } ?: true
                    ui.post {
                        if (!done) toast(activity.getString(R.string.library_move_failed))
                        reload()
                    }
                }
            }
            .show()
    }

    /**
     * Rename a folder: every file in it moves, and the memory of it moves too.
     *
     * Renaming and then reloading has a window where neither name exists, so
     * the chip that was in force follows the new name before the reload asks
     * storage what is true now.
     */
    private fun askToRenameFolder(name: String) {
        val field = EditText(activity)
        field.setText(name)
        field.setSelection(field.text.length)
        field.setSingleLine()
        MaterialAlertDialogBuilder(activity)
            .setTitle(R.string.library_rename)
            .setView(field)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.confirm) { _, _ ->
                val fresh = Library.clean(field.text.toString())
                if (fresh.isBlank() || fresh == name) return@setPositiveButton
                work.execute {
                    val moved = Library.renameFolder(activity, name, fresh, items, tab)
                    ui.post {
                        if (!moved) toast(activity.getString(R.string.library_rename_failed))
                        if (showing == name) showing = fresh
                        reload()
                    }
                }
            }
            .show()
    }

    private fun confirmRemoveFolder(name: String) {
        // Three choices: keep the files (fold them back to the top), or take the
        // folder and everything in it. "전체삭제" cannot be undone, so it is not
        // the default button.
        MaterialAlertDialogBuilder(activity)
            .setTitle(name)
            .setMessage(activity.getString(R.string.library_folder_remove_ask, name))
            .setNegativeButton(R.string.cancel, null)
            .setNeutralButton(R.string.library_folder_remove) { _, _ ->
                work.execute {
                    Library.removeFolder(activity, name, items, tab)
                    ui.post { if (showing == name) showing = null; reload() }
                }
            }
            .setPositiveButton(R.string.library_folder_delete_all) { _, _ ->
                work.execute {
                    items.filter { it.kind == tab && it.folder == name }.forEach { Library.delete(activity, it) }
                    ui.post { if (showing == name) showing = null; reload() }
                }
            }
            .show()
    }

    private fun toast(text: String) {
        Toast.makeText(activity, text, Toast.LENGTH_SHORT).show()
    }

    private companion object {
        const val MOVE_OUT = 1
        const val NEW_FOLDER = 2
        const val DELETE = 3

        /** The name, changed where it is written. */
        const val RENAME = 6

        /**
         * How long the controls stay up after being asked for.
         *
         * Five seconds, not three. Three is over before a hand that has just
         * seen the bar appear can reach the button it appeared for.
         */
        const val CONTROLS_MS = 5_000

        /**
         * How often the bar's scrubber and clock are moved along.
         *
         * Twice a second was visible as a stutter in the knob — it moved in
         * steps a fifth of a second wide while the film ran smoothly behind it.
         */
        const val TICK_MS = 200L

        /** How far a swipe has to travel, in density-independent pixels. */
        const val SWIPE_DP = 60

        /** How often the screen asks storage what is there now. */
        const val REFRESH_MS = 4_000L

        /** Folder entries are numbered from here, clear of the fixed ones. */
        const val FOLDER_BASE = 100

        /** How far a double tap jumps, forward or back. */
        const val SEEK_MS = 3_000

        /** How long after the last tap a run of seeks is considered over. */
        const val SEEK_SETTLE_MS = 700L
    }
}
