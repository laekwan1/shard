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
import android.widget.MediaController
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
    private val folderRow: LinearLayout = root.findViewById(R.id.folders)
    private val folderScroll: View = root.findViewById(R.id.folderScroll)
    private val newFolder: ImageButton = root.findViewById(R.id.newFolder)
    private val tabVideo: View = root.findViewById(R.id.tabVideo)
    private val tabVideoIcon: android.widget.ImageView = root.findViewById(R.id.tabVideoIcon)
    private val tabVideoText: TextView = root.findViewById(R.id.tabVideoText)
    private val tabMusic: View = root.findViewById(R.id.tabMusic)
    private val tabMusicIcon: android.widget.ImageView = root.findViewById(R.id.tabMusicIcon)
    private val tabMusicText: TextView = root.findViewById(R.id.tabMusicText)
    private val gauge: View = root.findViewById(R.id.gauge)
    private val gaugeIcon: android.widget.ImageView = root.findViewById(R.id.gaugeIcon)
    private val gaugeBar: android.widget.ProgressBar = root.findViewById(R.id.gaugeBar)
    private val nowPlaying: View = root.findViewById(R.id.nowPlaying)
    private val npArt: com.google.android.material.imageview.ShapeableImageView =
        root.findViewById(R.id.npArt)
    private val npTitle: TextView = root.findViewById(R.id.npTitle)
    private val npToggle: ImageButton = root.findViewById(R.id.npToggle)
    private val npStop: ImageButton = root.findViewById(R.id.npStop)
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
        override fun getItemId(at: Int) = shown()[at].id

        override fun getView(at: Int, reuse: View?, parent: ViewGroup): View {
            val view = reuse ?: LayoutInflater.from(activity)
                .inflate(R.layout.item_saved, parent, false).also { roundArt(it) }
            val item = shown()[at]
            view.findViewById<TextView>(R.id.name).text = item.title
            view.findViewById<TextView>(R.id.detail).text = describe(item)
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
    /** A soft rectangle, not a circle: what is in it is a picture, not a face. */
    private val tileCorner = 7f * activity.resources.displayMetrics.density
    private val tilePad = (14 * activity.resources.displayMetrics.density).toInt()
    private val mutedTint =
        android.content.res.ColorStateList.valueOf(activity.getColor(R.color.muted))

    /** Round the tile once, when the row is first inflated rather than every bind. */
    private fun roundArt(row: View) {
        val art = row.findViewById<com.google.android.material.imageview.ShapeableImageView>(R.id.art)
        art.shapeAppearanceModel =
            art.shapeAppearanceModel.toBuilder().setAllCornerSizes(tileCorner).build()
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
            if (bmp == null) {
                bmp = runCatching {
                    activity.contentResolver.loadThumbnail(item.uri, android.util.Size(200, 200), null)
                }.getOrNull()
            }
            if (bmp == null) return@execute
            thumbCache.put(item.uri, bmp)
            ui.post { if (art.tag == item.uri) showThumb(art, badge, bmp, music) }
        }
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
        art.scaleType = android.widget.ImageView.ScaleType.CENTER_CROP
        art.setImageBitmap(bmp)
        badge?.visibility = if (music) View.GONE else View.VISIBLE
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

    private val controls = MediaController(activity).apply {
        setMediaPlayer(object : MediaController.MediaPlayerControl {
            // The on-screen scrub bar and the notification are two faces of one
            // player, so a press on either has to move the intent and redraw the
            // other. Going straight to the player here — as the first version
            // did — left `wantsPlay` and the notification untouched, so pausing
            // from the screen showed the wrong button in the shade.
            override fun start() {
                player?.play()
                wantsPlay = true
                refreshMediaNotification()
            }

            override fun pause() {
                player?.pause()
                wantsPlay = false
                refreshMediaNotification()
            }

            override fun getDuration() = player?.duration?.takeIf { it > 0 }?.toInt() ?: 0
            override fun getCurrentPosition() = player?.currentPosition?.toInt() ?: 0

            override fun seekTo(where: Int) {
                // ExoPlayer is set to seek to the exact frame, so this lands where
                // the finger let go rather than on the previous keyframe.
                player?.seekTo(where.toLong())
            }

            // The intent, not the instant — the same value the notification's
            // button is drawn from. The controller picks its own play/pause
            // glyph and decides which way to toggle from this, so reading the
            // player's transient state here would let the on-screen button
            // flicker or fire the wrong way during a seek or just after a start.
            override fun isPlaying() = prepared && wantsPlay
            override fun getBufferPercentage() = player?.bufferedPercentage ?: 0
            override fun canPause() = true
            override fun canSeekBackward() = true
            override fun canSeekForward() = true
            override fun getAudioSessionId() = 0
        })
        setAnchorView(stage)
    }

    init {
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
            if (!pinch.isInProgress) gestures.onTouchEvent(event)
            true
        }
        EngineControl.stopPlayback = stopHook
        // The notification's play/pause button reaches the player through here.
        EngineControl.togglePlayback = toggleHook
        root.findViewById<ImageButton>(R.id.backFromLibrary).setOnClickListener { back() }
        newFolder.setOnClickListener { askForFolder() }
        root.findViewById<ImageButton>(R.id.librarySettings).setOnClickListener { showSettings() }
        expand.setOnClickListener { setExpanded(!expanded) }
        leaveFullScreen.setOnClickListener { setExpanded(false) }
        tabVideo.setOnClickListener { selectTab(Library.Kind.VIDEO) }
        tabMusic.setOnClickListener { selectTab(Library.Kind.MUSIC) }
        npToggle.setOnClickListener { togglePlayPause() }
        npStop.setOnClickListener { stopPlaying() }
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
        // Both shelves keep folders now — the names differ per shelf, so they are
        // recomputed for the one being shown.
        folders = runCatching { Library.folders(activity, items, kind) }.getOrDefault(emptyList())
        drawFolders()
        adapter.notifyDataSetChanged()
        list.setSelection(0)
        showEmpty()
    }

    /**
     * A shelf tab in the segmented control, lit when it is the one in force.
     *
     * The pill background follows the selected state on its own; this colours
     * the label and its icon — the accent when in force, muted when not — so the
     * active shelf reads at a glance without shouting a full accent fill.
     */
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
        emptyText.setText(if (music) R.string.library_empty_music else R.string.library_empty)
        emptyIcon.setImageResource(if (music) R.drawable.ic_music else R.drawable.ic_video)
        empty.visibility = if (shown().isEmpty()) View.VISIBLE else View.GONE
    }

    // ---- coming and going --------------------------------------------------

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
        // Something left playing when the library was last closed is still going,
        // so its player comes back with the screen — but only onto the shelf it
        // belongs to.
        syncPlayerView()
    }

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
            stage.visibility = View.VISIBLE
            stage.layoutParams = stage.layoutParams.apply { height = stageHeight() }
            expand.visibility = View.VISIBLE
            // The surface went with the stage when it was hidden, so the player
            // is handed it again — otherwise the sound plays on over a picture
            // that never comes back.
            player?.setVideoSurfaceView(surface)
            stage.post { fitSurface() }
        } else {
            stage.visibility = View.GONE
            // The scrub bar belongs to the picture; leaving it up over another
            // shelf is what made it linger and then vanish on its own.
            controls.hide()
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
        controls.show(CONTROLS_MS)
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
        return showing?.let { name -> shelf.filter { it.folder == name } } ?: shelf
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

            // Confirmed rather than raw, so the first tap of a double tap does
            // not flash the controls on its way to a seek.
            override fun onSingleTapConfirmed(e: android.view.MotionEvent): Boolean {
                if (controls.isShowing) controls.hide() else controls.show(CONTROLS_MS)
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
        // A song has no picture. It plays in the bar at the top, and the video
        // stage — its surface, its frame, its full screen — is left out of it
        // entirely, so the last video's frame never shows behind a note and the
        // player never touches the machinery a picture needs.
        npFor = null
        syncPlayerView()

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

        exo.addListener(object : Player.Listener {
            override fun onPlaybackStateChanged(state: Int) {
                if (player !== exo) return
                when (state) {
                    Player.STATE_READY -> if (!prepared) onPlayerReady(item)
                    Player.STATE_ENDED -> onPlaybackEnded()
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
            controls.show(CONTROLS_MS)
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
            controls.show(CONTROLS_MS)
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
            controls.show(CONTROLS_MS)
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
        controls.hide()
        playing = null
        expanded = false
        activity.requestedOrientation = android.content.pm.ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        chrome.visibility = View.VISIBLE
        stage.visibility = View.GONE
        expand.visibility = View.VISIBLE
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
        return minOf(metrics.widthPixels * 9 / 16, metrics.heightPixels / 2)
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
        chrome.visibility = if (expanded) View.GONE else View.VISIBLE
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
        // about is not a way out.
        leaveFullScreen.visibility = if (expanded) View.VISIBLE else View.GONE
        stage.post { fitSurface() }
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
        val menu = PopupMenu(activity, anchor)
        if (item.folder.isNotBlank()) {
            menu.menu.add(0, MOVE_OUT, 0, R.string.library_move_out)
        }
        folders.filter { it != item.folder }.forEachIndexed { at, name ->
            menu.menu.add(1, FOLDER_BASE + at, 1, name)
        }
        menu.menu.add(0, NEW_FOLDER, 2, R.string.library_new_folder)
        // Where this one should start from. Offered on the file that is playing,
        // and the way back offered on any file that has been given one.
        if (playing?.id == item.id) menu.menu.add(0, HOLD, 3, R.string.library_hold)
        if (Library.holdAt(activity, item) > 0) {
            menu.menu.add(0, FORGET_HOLD, 4, R.string.library_hold_forget)
        }
        menu.menu.add(0, DELETE, 5, R.string.library_delete)
        menu.setOnMenuItemClickListener { chosen ->
            when (val id = chosen.itemId) {
                MOVE_OUT -> moveTo(item, "")
                NEW_FOLDER -> askForFolder(item)
                HOLD -> holdHere(item)
                FORGET_HOLD -> forgetHold(item)
                DELETE -> confirmDelete(item)
                else -> if (id >= FOLDER_BASE) {
                    val names = folders.filter { it != item.folder }
                    names.getOrNull(id - FOLDER_BASE)?.let { moveTo(item, it) }
                }
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
            .setNegativeButton(android.R.string.cancel, null)
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
        folderRow.addView(chip(activity.getString(R.string.library_all), showing == null, "") {
            showing = null
            drawFolders()
            adapter.notifyDataSetChanged()
            showEmpty()
        })
        folders.forEach { name ->
            val chip = chip(name, showing == name, name) {
                showing = if (showing == name) null else name
                drawFolders()
                adapter.notifyDataSetChanged()
                showEmpty()
            }
            chip.setOnLongClickListener {
                confirmRemoveFolder(name)
                true
            }
            folderRow.addView(chip)
        }
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
        // A tab, not a pill: the label on an accent underline when it is the one
        // in force, plain otherwise — the row reads like a browser's tabs.
        view.setBackgroundResource(R.drawable.folder_tab)
        view.setTextColor(activity.getColor(if (on) R.color.on_surface else R.color.muted))
        view.textSize = 14f
        view.setTypeface(
            view.typeface,
            if (on) android.graphics.Typeface.BOLD else android.graphics.Typeface.NORMAL,
        )
        val padH = (12 * density).toInt()
        val padV = (9 * density).toInt()
        view.setPadding(padH, padV, padH, padV)
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
            .setNegativeButton(android.R.string.cancel, null)
            .setPositiveButton(android.R.string.ok) { _, _ ->
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

    private fun confirmRemoveFolder(name: String) {
        MaterialAlertDialogBuilder(activity)
            .setMessage(activity.getString(R.string.library_folder_remove_ask, name))
            .setNegativeButton(android.R.string.cancel, null)
            .setPositiveButton(R.string.library_folder_remove) { _, _ ->
                work.execute {
                    Library.removeFolder(activity, name, items, tab)
                    ui.post {
                        if (showing == name) showing = null
                        reload()
                    }
                }
            }
            .show()
    }

    /**
     * The library's own settings.
     *
     * A dialog rather than a menu, because a menu closes on the first thing
     * pressed — and a switch that puts its own panel away as it is switched
     * gives no chance to see what it did. This one stays until it is stepped
     * away from.
     */
    private fun showSettings() {
        val view = LayoutInflater.from(activity)
            .inflate(R.layout.dialog_library_settings, null)

        val background = view.findViewById<android.widget.CompoundButton>(R.id.settingBackground)
        background.isChecked = Library.backgroundPlayback(activity)
        // Applied as it is toggled, so the dialog has no OK button to forget to
        // press — the settings are the state, not a form to submit.
        background.setOnCheckedChangeListener { _, checked ->
            Library.setBackgroundPlayback(activity, checked)
        }

        val group = view.findViewById<android.widget.RadioGroup>(R.id.settingEnd)
        group.check(
            when (Library.playbackEnd(activity)) {
                Library.PlaybackEnd.STOP -> R.id.endStop
                Library.PlaybackEnd.NEXT -> R.id.endNext
                Library.PlaybackEnd.SHUFFLE -> R.id.endShuffle
            }
        )
        group.setOnCheckedChangeListener { _, checkedId ->
            Library.setPlaybackEnd(
                activity,
                when (checkedId) {
                    R.id.endNext -> Library.PlaybackEnd.NEXT
                    R.id.endShuffle -> Library.PlaybackEnd.SHUFFLE
                    else -> Library.PlaybackEnd.STOP
                },
            )
        }

        MaterialAlertDialogBuilder(activity)
            .setTitle(R.string.library_settings)
            .setView(view)
            .setPositiveButton(R.string.close, null)
            .show()
    }

    private fun toast(text: String) {
        Toast.makeText(activity, text, Toast.LENGTH_SHORT).show()
    }

    private companion object {
        const val MOVE_OUT = 1
        const val NEW_FOLDER = 2
        const val DELETE = 3

        /** Start this one where it is now, and take that back. */
        const val HOLD = 4
        const val FORGET_HOLD = 5

        /** How long the controls stay up after being asked for. */
        const val CONTROLS_MS = 3_000

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
