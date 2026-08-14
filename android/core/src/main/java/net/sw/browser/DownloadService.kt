package net.sw.browser

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.widget.RemoteViews
import java.util.concurrent.Executors

/**
 * Downloads that keep going when the browser is not on screen.
 *
 * A foreground service is the only way Android allows that, and it earns its
 * keep twice over: the notification it is obliged to show is also the place the
 * engine can be switched and the progress read without opening the app. And
 * because the engine runs inside this process, keeping the process alive is
 * exactly what keeps the proxy the download is going through alive too.
 */
class DownloadService : Service() {

    private val workers = Executors.newFixedThreadPool(2)
    private val ui = Handler(Looper.getMainLooper())
    private var ticking = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannel()
        // The library tells this service when its video started, paused or
        // stopped, so the one notification can carry both the engine switch and
        // the playback controls.
        EngineControl.onPlaybackChanged = { ui.post { refresh() } }
        // A foreground service from the start.
        //
        // An ordinary notification can be swiped away, and this one is a switch
        // — losing it means going back into the app to find it. A foreground
        // service's notification cannot be dismissed, which is the behaviour
        // wanted, and it goes when the process goes, which is the other half:
        // the switch should not outlive the app it switches.
        startForeground(NOTIFICATION_ID, buildNotification())
        foreground = true
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Anything started as a foreground service has a few seconds to become
        // one, so this is settled before the work is looked at.
        when (intent?.action) {
            ACTION_TOGGLE -> {
                EngineControl.toggle()
                refresh()
            }
            ACTION_DOWNLOAD -> pending.removeFirstOrNull()?.let { begin(it) }
            ACTION_RESTORE -> refresh()
            // No refresh here: the player runs the command, then reports back
            // through onPlaybackChanged, which redraws with the new state.
            // Refreshing now would draw the old state a frame before the new.
            ACTION_PB_TOGGLE -> EngineControl.togglePlayback?.invoke()
            ACTION_PB_STOP -> EngineControl.stopPlayback?.invoke()
        }
        startTicking()
        // Not restarted by the system after the app is gone. The notification
        // is the app's switch; without the app there is nothing to switch.
        return START_NOT_STICKY
    }

    /**
     * The app was swiped out of the recents list.
     *
     * That is the user saying they are done, so the switch goes with it rather
     * than sitting in the shade driving a process they thought they had closed.
     */
    override fun onTaskRemoved(rootIntent: Intent?) {
        // Downloads go with it. Closing the app and finding it still using the
        // line — and still in the shade — is the app deciding it knows better
        // than the person who closed it.
        Downloads.snapshot.forEach { Downloads.cancel(it) }
        // And anything still playing. Sound coming from an app that was closed
        // is the app arguing with the person who closed it.
        runCatching { EngineControl.stopPlayback?.invoke() }
        going = true
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        super.onTaskRemoved(rootIntent)
    }

    /** Set once the app has gone, so nothing puts the notification back. */
    private var going = false

    /** Whether this service has been promoted for a download. */
    private var foreground = false

    /** Run one download and keep its job updated. */
    private fun begin(request: Request) {
        val job = Downloads.start(request.media.title.ifBlank { "영상" })
        job.total = request.expectedBytes
        refresh()

        workers.execute {
            val result = runCatching {
                val downloader = Downloader(applicationContext)
                downloader.onStage = { stage -> job.joining = stage == Downloader.Stage.JOINING }
                downloader.fetch(request.media, { job.cancelled }) { progress ->
                    job.advance(progress.bytes, job.total, android.os.SystemClock.elapsedRealtime())
                }
            }
            // A cancelled job has already left the list; reporting a failure on
            // it would put the row back to say the user got what they asked for.
            if (job.cancelled) {
                ui.post { refresh(); stopIfIdle() }
                return@execute
            }
            Downloads.finish(job, result.exceptionOrNull()?.message)
            ui.post {
                refresh()
                // Leave the finished entry up long enough to be seen, then let
                // the service go if there is nothing else to do.
                ui.postDelayed({
                    Downloads.forget(job)
                    refresh()
                    stopIfIdle()
                }, FINISHED_LINGER_MS)
            }
        }
    }

    /** One update a second while anything is running. */
    private fun startTicking() {
        if (ticking) return
        ticking = true
        ui.post(object : Runnable {
            override fun run() {
                refresh()
                if (Downloads.running.isNotEmpty()) ui.postDelayed(this, 1000) else ticking = false
            }
        })
    }

    /**
     * Come back down to an ordinary notification when the work is done.
     *
     * The service is not stopped and the notification is not taken away: the
     * switch in it is the point, and it is wanted when nothing is downloading
     * as much as when something is.
     */
    /**
     * Nothing is stopped when the downloads finish.
     *
     * The notification is the switch, and it is wanted when nothing is
     * downloading as much as when something is. What ends it is the app ending
     * — see `onTaskRemoved`.
     */
    private fun stopIfIdle() {
        refresh()
    }

    /** Fires when the notification is swiped away, so it can be put back. */
    private fun restorePendingIntent(): android.app.PendingIntent {
        val intent = Intent(this, DownloadService::class.java).setAction(ACTION_RESTORE)
        return android.app.PendingIntent.getService(
            this,
            2,
            intent,
            android.app.PendingIntent.FLAG_UPDATE_CURRENT or
                android.app.PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun refresh() {
        if (going) return
        val manager = getSystemService(NotificationManager::class.java)
        manager.notify(NOTIFICATION_ID, buildNotification())
    }

    // ---- the notification --------------------------------------------------

    private fun createChannel() {
        val channel = NotificationChannel(
            CHANNEL,
            getString(R.string.channel_downloads),
            // Low: this is a running indicator, not an event worth a sound.
            NotificationManager.IMPORTANCE_LOW,
        ).apply { setShowBadge(false) }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun buildNotification(): Notification {
        return Notification.Builder(this, CHANNEL)
            // The app's own mark rather than a download arrow: the row is the
            // app's switch, and it is there when nothing is downloading.
            .setSmallIcon(R.drawable.ic_notification)
            // The circle the phone draws behind that silhouette takes this
            // colour, so the badge is the app's cyan rather than a system blue.
            .setColor(getColor(R.color.notification_tint))
            .setCustomContentView(row(R.layout.notification_bar, big = false))
            .setCustomBigContentView(row(R.layout.notification_bar_big, big = true))
            .setStyle(Notification.DecoratedCustomViewStyle())
            // Kept in the shade rather than swiped away by accident: it is a
            // switch, and a switch that can be lost is a switch that has to be
            // found again through the app.
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(openAppPendingIntent())
            // Put back when swiped away. From Android 14 even a foreground
            // service's notification can be dismissed, and this one is a switch
            // — losing it means going back into the app to find it. It goes
            // when the app goes, which is `onTaskRemoved`, and not before.
            .setDeleteIntent(restorePendingIntent())
            .build()
    }

    /**
     * Fill in one of the two notification layouts.
     *
     * The collapsed and expanded rows share everything but one thing: the
     * expanded one has room for the playing video's title on its own line, the
     * collapsed one does not show it at all. `big` is that difference.
     */
    private fun row(layout: Int, big: Boolean): RemoteViews = RemoteViews(packageName, layout).apply {
        val running = Downloads.running
        val overall = Downloads.overallPercent()
        val engineOn = EngineControl.isOn()
        val playingTitle = EngineControl.playbackTitle

        setImageViewResource(R.id.power, R.drawable.ic_power)
        // Coloured when running, grey when not — the same read as the button in
        // the app. A dimmed accent and a bright one are the same colour seen at
        // two distances.
        setInt(R.id.power, "setAlpha", if (engineOn) 255 else 200)
        setInt(R.id.power, "setColorFilter", getColor(if (engineOn) R.color.accent else R.color.muted))
        setOnClickPendingIntent(R.id.power, togglePendingIntent())

        // The library's playback controls, in their own framed box, when a video
        // is playing. Play/pause and stop, both, so the shade can do either.
        if (playingTitle != null) {
            setViewVisibility(R.id.pbFrame, android.view.View.VISIBLE)
            setImageViewResource(
                R.id.pbToggle,
                if (EngineControl.playbackPlaying) R.drawable.ic_pb_pause else R.drawable.ic_pb_play,
            )
            setImageViewResource(R.id.pbStop, R.drawable.ic_pb_stop)
            // A mid grey, not the app's near-white text colour: the shade
            // follows the system theme, and a near-white icon vanishes on a
            // light one. Grey reads on both.
            setInt(R.id.pbToggle, "setColorFilter", getColor(R.color.muted))
            setInt(R.id.pbStop, "setColorFilter", getColor(R.color.muted))
            setOnClickPendingIntent(R.id.pbToggle, actionPendingIntent(ACTION_PB_TOGGLE, 3))
            setOnClickPendingIntent(R.id.pbStop, actionPendingIntent(ACTION_PB_STOP, 4))
        } else {
            setViewVisibility(R.id.pbFrame, android.view.View.GONE)
        }

        // The title belongs to the expanded view only, on its own line. The
        // collapsed row shows no title — the buttons are enough there.
        if (big) {
            if (playingTitle != null) {
                setViewVisibility(R.id.pbTitle, android.view.View.VISIBLE)
                setTextViewText(R.id.pbTitle, playingTitle)
            } else {
                setViewVisibility(R.id.pbTitle, android.view.View.GONE)
            }
        }

        // The middle line names the download, if one is running and no video is
        // playing. A playing video says its name in the expanded view's title
        // line, not here.
        setTextViewText(
            R.id.label,
            when {
                playingTitle != null -> ""
                running.isEmpty() -> ""
                running.size == 1 -> running.first().name
                else -> getString(R.string.downloading_count, running.size)
            },
        )

        // The gauge is the download's; it steps aside while a video names the
        // row, so its percentage is never read as the video's.
        if (playingTitle != null || running.isEmpty()) {
            setViewVisibility(R.id.gauge, android.view.View.GONE)
            setTextViewText(R.id.percent, "")
        } else {
            setViewVisibility(R.id.gauge, android.view.View.VISIBLE)
            setProgressBar(R.id.gauge, 100, overall ?: 0, overall == null)
            setTextViewText(R.id.percent, overall?.let { "$it%" } ?: "")
        }
    }

    private fun togglePendingIntent(): PendingIntent {
        val intent = Intent(this, DownloadService::class.java).setAction(ACTION_TOGGLE)
        return PendingIntent.getService(
            this, 1, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun actionPendingIntent(action: String, request: Int): PendingIntent {
        val intent = Intent(this, DownloadService::class.java).setAction(action)
        return PendingIntent.getService(
            this, request, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun openAppPendingIntent(): PendingIntent {
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        return PendingIntent.getActivity(
            this, 2, launch,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    override fun onDestroy() {
        workers.shutdownNow()
        ui.removeCallbacksAndMessages(null)
        EngineControl.onPlaybackChanged = null
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "downloads"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_TOGGLE = "net.sw.browser.TOGGLE"
        private const val ACTION_DOWNLOAD = "net.sw.browser.DOWNLOAD"
        private const val ACTION_PB_TOGGLE = "net.sw.browser.PB_TOGGLE"
        private const val ACTION_PB_STOP = "net.sw.browser.PB_STOP"
        private const val FINISHED_LINGER_MS = 4000L

        private class Request(val media: Media, val expectedBytes: Long)

        /**
         * Handed over in memory rather than through the intent.
         *
         * The service is in this process, and a `Media` carries a map of
         * headers that would have to be flattened and rebuilt to travel as
         * extras — work that could only introduce a difference between what was
         * chosen and what is fetched.
         */
        private const val ACTION_RESTORE = "restore"

        private val pending = ArrayDeque<Request>()

        /** Queue a download and make sure the service is running. */
        fun enqueue(context: Context, media: Media, expectedBytes: Long) {
            synchronized(pending) { pending.addLast(Request(media, expectedBytes)) }
            val intent = Intent(context, DownloadService::class.java).setAction(ACTION_DOWNLOAD)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        /**
         * Put the switch in the shade.
         *
         * Started plainly rather than as a foreground service: there is nothing
         * to do yet, and promising the system a foreground service that does no
         * work is how an app gets stopped for lying. Called while the activity
         * is on screen, which is when an ordinary start is allowed.
         */
        fun showControls(context: Context) {
            val intent = Intent(context, DownloadService::class.java)
            runCatching {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            }
        }
    }
}
