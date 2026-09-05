package net.sw.browser

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.EditText
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.core.view.doOnLayout
import androidx.core.view.updateLayoutParams
import androidx.lifecycle.lifecycleScope
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import net.sw.browser.databinding.ActivityBrowserBinding

/**
 * A browser with an engine underneath it.
 *
 * Both apps are this screen; they differ only in what [startEngine] launches.
 * Putting the browser inside the app is what makes the rest simple — no VPN
 * permission, no system-wide tunnel, nothing left running when the app is
 * closed, and only what is browsed here is affected. It also puts the app in a
 * position to see the video requests a page makes, which is what makes
 * downloading possible at all.
 */
abstract class BrowserActivity : AppCompatActivity() {

    protected lateinit var binding: ActivityBrowserBinding
        private set

    private val catcher = MediaCatcher()
    private val ui = Handler(Looper.getMainLooper())
    private var customView: View? = null
    private var customViewCallback: WebChromeClient.CustomViewCallback? = null


    /**
     * The current page's title, kept here rather than read from the web view.
     *
     * Requests are intercepted on a background thread, and every WebView method
     * — `title` included — throws if called off the main thread. Mirroring the
     * value as the page reports it is what lets the interceptor name a download
     * without touching the view.
     */
    @Volatile
    private var pageTitle: String = ""

    /** The pinned sites and the history — what the favorites overlay draws. */
    private val favorites by lazy { Favorites(this) }

    /** Whether the favorites overlay is up (the star toggles it). */
    private var favoritesShown = false

    private var panelShown = false

    /**
     * The id of the short the Shorts reel was entered on — its first video.
     *
     * On that first short a downward swipe has no previous short to step to, so it
     * should reload (user ask); on any later short the same swipe steps back one and
     * must not reload. Knowing which short is the entry is the whole difference, and
     * the reel keeps no "previous" above the one it opened on, so entry == first.
     * Null when not in Shorts, so leaving and re-entering captures a fresh first.
     */
    private var shortsEntryId: String? = null

    /** The video id in a `/shorts/<id>` url, or null when the url is not a short. */
    private fun shortsIdOf(url: String?): String? {
        val u = url ?: return null
        val at = u.indexOf("/shorts/")
        if (at < 0) return null
        val rest = u.substring(at + 8)
        val end = rest.indexOfFirst { it == '?' || it == '/' || it == '#' || it == '&' }
        return (if (end < 0) rest else rest.substring(0, end)).ifEmpty { null }
    }

    /**
     * Remember the first short of a reel, and forget it on the way out.
     *
     * The first `/shorts/` url after arriving is the entry; later shorts keep that
     * same entry so only the true first video enables the reload swipe.
     */
    private fun noteShortsUrl(url: String?) {
        val id = shortsIdOf(url)
        when {
            id == null -> shortsEntryId = null
            shortsEntryId == null -> shortsEntryId = id
        }
    }

    /**
     * Drag down from the top of a page to reload it.
     *
     * Made after the layout exists, since it parks its own indicator.
     */
    private val pull by lazy {
        PullToRefresh(
            indicator = binding.pull,
            // The page's own scroll position, not a gesture's: a drag that
            // starts partway down a page is a scroll and must stay one.
            atTop = { binding.web.scrollY == 0 },
            onRefresh = { binding.web.reload() },
        )
    }

    /**
     * What has been saved, as a screen of its own over the browser.
     *
     * Made once and kept: it holds a player, and building one per opening would
     * mean waiting for it every time the library is looked at.
     */
    private val library by lazy {
        libraryMade = true
        LibraryScreen(this, binding.root).apply {
            onOpen = { pauseWebPlayback() }
            onClose = { resumeWebPlayback() }
        }
    }

    /**
     * Pause the page's own playback while the library is over it.
     *
     * A video or a song from the library should not fight the page's for the
     * speakers. Only what was actually playing is marked and stopped, and the
     * position is kept, so leaving the library plays it on from where it paused.
     */
    private fun pauseWebPlayback() {
        // Pause the media first, then suspend the web view — in that order, and
        // suspending only once the pause script has run, so onPause() does not
        // cut the script off before it quiets the sound.
        binding.web.evaluateJavascript(
            "document.querySelectorAll('video,audio').forEach(function(m){" +
                "if(!m.paused){m.setAttribute('data-shardpaused','1');m.pause();}});",
        ) { binding.web.onPause() }
    }

    /** Let the page carry on from where [pauseWebPlayback] stopped it. */
    private fun resumeWebPlayback() {
        binding.web.onResume()
        binding.web.evaluateJavascript(
            "document.querySelectorAll('[data-shardpaused]').forEach(function(m){" +
                "m.removeAttribute('data-shardpaused');m.play();});",
            null,
        )
    }

    /** Whether the library has ever been opened, so the lifecycle can skip it. */
    private var libraryMade = false

    /**
     * The page waiting on a file, while the picker is open.
     *
     * A web view does nothing at all when a page asks for a file and the app
     * has not said how to ask for one — no picker, no error, no clue. Every
     * attachment button on every site is dead until this exists.
     */
    private var awaitingFiles: android.webkit.ValueCallback<Array<Uri>>? = null

    private val filePicker = registerForActivityResult(
        androidx.activity.result.contract.ActivityResultContracts.StartActivityForResult()
    ) { result ->
        val waiting = awaitingFiles ?: return@registerForActivityResult
        awaitingFiles = null
        // Cancelled has to be answered too. A callback left unanswered leaves
        // the page's file input jammed for good — the next press does nothing.
        waiting.onReceiveValue(filesFrom(result.resultCode, result.data))
    }

    /** What came back from the picker: several files, one, or none. */
    private fun filesFrom(code: Int, data: Intent?): Array<Uri>? {
        if (code != android.app.Activity.RESULT_OK || data == null) return null
        data.clipData?.let { clip ->
            return Array(clip.itemCount) { clip.getItemAt(it).uri }
        }
        return data.data?.let { arrayOf(it) }
    }
    /** Previous counters, for turning totals into a rate. */
    private var lastBytes = 0L
    private var lastTick = 0L
    private var lastRate = 0.0
    private var lastRateAt = 0L

    /** True while a download sheet is up, so a page cannot stack them. */
    private var choosing = false

    /** Whether the YouTube recorder is injected before the page's own scripts. */
    private var recorderRunsAtStart = false

    var engineOn: Boolean = false
        private set

    // ---- what each app supplies -------------------------------------------

    /** Start the engine and return its local port, or a value <= 0 on failure. */
    protected abstract fun startEngine(): Int

    /**
     * How the browser should reach the engine.
     *
     * Tor speaks SOCKS and needs names resolved at the far end; everything else
     * here is a local HTTP proxy.
     */
    protected open fun proxyKind(): ProxyRoute.Kind = ProxyRoute.Kind.HTTP

    protected abstract fun stopEngine()

    /** A short word for what the engine is doing, e.g. "우회 중". */
    protected abstract fun engineStatus(): String

    /** Total bytes received so far, for working out a rate. */
    protected abstract fun bytesReceived(): Long

    /**
     * How the notification switches the engine.
     *
     * Built against the application context, never this activity: the
     * notification outlives the screen, and a control holding an activity would
     * keep a dead one alive.
     */
    protected abstract fun engineControl(): EngineControl.Control

    /** Shown when the engine could not start. */
    protected open fun startFailureMessage(code: Int) = "엔진을 시작할 수 없습니다 (코드 $code)"

    /** Whether to bring the engine up as soon as the app opens. */
    protected open val autoStart: Boolean get() = true

    /**
     * Where the browser opens.
     *
     * YouTube rather than a search box: this browser exists to save video, and
     * the page it is nearly always pointed at is the one it can save from. What
     * is typed into the address bar is still answered by Google — see [asUrl].
     * The two are separate questions and the earlier code answered both with
     * one URL.
     */
    protected open val homeUrl: String get() = "https://m.youtube.com/"

    // ---- lifecycle ---------------------------------------------------------

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        binding = ActivityBrowserBinding.inflate(layoutInflater)
        setContentView(binding.root)

        applyWindowInsets()
        configureWebView()
        wireControls()
        setUpPanel()
        handleBackPresses()

        if (!ProxyRoute.supported) {
            toast("이 기기의 WebView는 프록시 설정을 지원하지 않습니다")
        }
        // The switch in the notification goes through the same path the button
        // in the app does. Pointed straight at the native engine it started it
        // and nothing else: the web views kept going out directly, because the
        // proxy override is set here and not there, and the button still said
        // off because nothing had told it otherwise.
        EngineControl.control = object : EngineControl.Control {
            override fun isOn() = engineOn
            override fun setOn(on: Boolean) {
                runOnUiThread { setEngine(on) }
            }
        }
        // The switch goes into the shade before the page is asked for.
        //
        // It used to be the last thing done, behind starting the engine and
        // loading a page — both of which take a moment — so the row appeared
        // seconds after the app did, which reads as it not working.
        DownloadService.showControls(applicationContext)
        askForNotifications()
        askForMedia()
        warnIfNotificationsAreOff()

        if (autoStart) setEngine(true) else showEngineState(false)
        configureStartOverlay()
        // First launch opens the web homepage, the same as iOS — not a start page.
        // The favorites page is an overlay the star toggles, not where the app lands.
        binding.web.loadUrl(favorites.homepage())
        ui.post(statusTicker)
    }

    /**
     * The notification is where downloads report from, so ask once.
     *
     * Declining costs the notification, not the download — the service still
     * runs, it just cannot say so.
     */
    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) ==
            android.content.pm.PackageManager.PERMISSION_GRANTED
        ) return
        requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
    }

    /**
     * Read back what earlier installs saved.
     *
     * Scoped storage lets an app read only its own media, and an uninstall
     * makes the store forget who owned the old files — so a reinstall opens onto
     * an empty library though the files are still there. This asks for the read
     * that makes them visible again. Declining costs only the old files; new
     * downloads still list, because the app can always read what it just made.
     */
    private val mediaPermissions: Array<String>
        get() = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            arrayOf(
                android.Manifest.permission.READ_MEDIA_VIDEO,
                android.Manifest.permission.READ_MEDIA_AUDIO,
            )
        } else {
            arrayOf(android.Manifest.permission.READ_EXTERNAL_STORAGE)
        }

    private fun askForMedia() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return
        val missing = mediaPermissions.filter {
            checkSelfPermission(it) != android.content.pm.PackageManager.PERMISSION_GRANTED
        }
        if (missing.isNotEmpty()) requestPermissions(missing.toTypedArray(), 2)
    }

    /**
     * Put the switch up once permission arrives.
     *
     * It is asked for and the notification is posted in the same breath, so on
     * a fresh install the posting loses the race — the permission is not
     * granted yet and nothing appears. Nothing posts it again either, because
     * an idle service has no reason to. So this does.
     */
    override fun onRequestPermissionsResult(
        code: Int,
        permissions: Array<out String>,
        results: IntArray,
    ) {
        super.onRequestPermissionsResult(code, permissions, results)
        if (code == 2) {
            // A media read just arrived: the library, if it is open, was showing
            // an empty or partial list built without it. Read the store again.
            if (results.any { it == android.content.pm.PackageManager.PERMISSION_GRANTED } &&
                libraryMade
            ) {
                library.refresh()
            }
            return
        }
        if (results.any { it == android.content.pm.PackageManager.PERMISSION_GRANTED }) {
            DownloadService.showControls(applicationContext)
        }
    }

    /**
     * A saved video stops when the app does.
     *
     * `library` is created on first use, so this asks whether it exists before
     * touching it — opening the library from `onPause` would be absurd.
     */
    override fun onPause() {
        if (libraryMade) library.pause()
        super.onPause()
    }

    override fun onResume() {
        super.onResume()
        if (libraryMade) library.resume()
    }

    override fun onDestroy() {
        ui.removeCallbacksAndMessages(null)
        // The switch was pointed at this activity; leaving it pointed at a dead
        // one would be a leak and a button that does nothing.
        EngineControl.control = null
        // The player outlives its own screen on purpose, so it has to be let go
        // by hand — otherwise closing the app leaves sound coming from it.
        EngineControl.stopPlayback = null
        if (libraryMade) library.release()
        // The engine is deliberately left running: a download in the service is
        // still going through it, and the notification can still switch it.
        super.onDestroy()
    }

    /**
     * Keep the toolbar and status line clear of the system bars.
     *
     * From Android 15 an app targeting SDK 35 is drawn edge to edge whether it
     * asks to be or not, so without this the address bar sits underneath the
     * clock. Only the browser chrome is inset — a video playing full screen
     * should cover the whole display.
     */
    private fun applyWindowInsets() {
        ViewCompat.setOnApplyWindowInsetsListener(binding.shell) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())

            // Lift the content above the keyboard only when the keyboard
            // belongs to the page. Typing in the address bar — which sits at
            // the top and is never covered — would otherwise reflow the whole
            // page and resize whatever is playing, for no benefit.
            val bottom = if (binding.url.hasFocus()) bars.bottom else maxOf(bars.bottom, ime.bottom)
            view.setPadding(bars.left, bars.top, bars.right, bottom)
            insets
        }
        // Focus decides the rule above, so a change of focus has to re-run it.
        binding.url.setOnFocusChangeListener { _, _ -> ViewCompat.requestApplyInsets(binding.shell) }
    }

    // ---- engine ------------------------------------------------------------

    fun setEngine(on: Boolean) {
        if (on == engineOn) return
        if (on) {
            val port = startEngine()
            if (port <= 0) {
                // Clear the route as well as reporting it. A previous engine's
                // override survives a failed start, which leaves the browser
                // quietly using the old one while the button says off.
                ProxyRoute.disable { }
                startFailureMessage(port).takeIf { it.isNotBlank() }?.let { toast(it) }
                return
            }
            // Reloading once the override is live is what makes the switch take
            // effect on the page already open, rather than only on the next one.
            ProxyRoute.enable(port, proxyKind()) { binding.web.reload() }
        } else {
            ProxyRoute.disable { binding.web.reload() }
            stopEngine()
        }
        engineOn = on
        showEngineState(on)
    }

    /**
     * Say when the shade has been shut for this app.
     *
     * Permission is per app, and Android stops asking after it has been
     * refused twice — so an app installed and declined once has no notification
     * and no way to be told why, which reads as the notification being broken.
     * The setting is the only way back, so the setting is what is named.
     */
    private fun warnIfNotificationsAreOff() {
        val manager = getSystemService(android.app.NotificationManager::class.java) ?: return
        if (manager.areNotificationsEnabled()) return
        toast(getString(R.string.notifications_off))
    }

    /**
     * What the status line says while the engine is off.
     *
     * Open for an app whose engine takes a while to come up: "off" is wrong
     * while something is starting, and there is nowhere else to say so.
     */
    protected open fun idleStatus(): String = getString(R.string.status_idle)

    /**
     * Coloured when running, grey when not.
     *
     * Dimming the accent colour was not enough to tell apart on a dark screen:
     * a faint blue and a bright blue are the same thing seen at two distances,
     * and the question being asked is not "how strongly" but "whether".
     */
    private fun showEngineState(on: Boolean) {
        binding.power.alpha = if (on) 1f else 0.8f
        binding.power.imageTintList = android.content.res.ColorStateList.valueOf(
            getColor(if (on) R.color.accent else R.color.muted)
        )
        // The ring around the glyph too. Half of it going grey and half of it
        // staying lit reads as neither state.
        binding.power.setBackgroundResource(
            if (on) R.drawable.power_backdrop else R.drawable.power_backdrop_off
        )
        binding.power.contentDescription = getString(if (on) R.string.on else R.string.off)
    }

    /**
     * What the status line says.
     *
     * Connection counts were in here and meant nothing: whether a page loaded
     * is visible on the page. What is not visible is how fast it is arriving,
     * which is the question actually worth asking of a tunnel — so that is what
     * this shows, alongside one word for what the engine is doing.
     */
    // The status line under the address bar was removed — it repeated what the
    // page already shows and what the download row/notification carry. The tick
    // stays only to keep the download row current.
    private val statusTicker = object : Runnable {
        override fun run() {
            refreshDownloads()
            ui.postDelayed(this, 1000)
        }
    }

    /**
     * Speed while anything is moving, session total when nothing is.
     *
     * Browsing is bursty: a page loads in a second and then the line is silent,
     * so a plain rate reads "—" almost always. The last rate is held briefly so
     * it can actually be read, and once things are properly idle the total says
     * something rather than nothing.
     */
    private fun throughput(): String {
        val now = android.os.SystemClock.elapsedRealtime()
        val bytes = bytesReceived()
        val elapsed = now - lastTick

        val rate = if (lastTick == 0L || elapsed <= 0) 0.0
        else (bytes - lastBytes) * 1000.0 / elapsed
        lastTick = now
        lastBytes = bytes

        if (rate >= 1024) {
            lastRate = rate
            lastRateAt = now
        }
        return when {
            now - lastRateAt < RATE_HOLD_MS -> "↓ ${formatRate(lastRate)}"
            bytes > 0 -> "· ${formatBytes(bytes)} 받음"
            else -> "· 대기"
        }
    }

    private fun formatRate(rate: Double) = when {
        rate >= 1 shl 20 -> "%.1f MB/s".format(rate / (1 shl 20))
        else -> "%.0f KB/s".format(rate / 1024)
    }

    /** Formerly replaced the status line for downloads; the line is gone, so
     *  this is now a no-op kept for the download callers and Veil's override. */
    protected fun showStatus(text: String) {
        // Intentionally empty — the address-bar status line was removed.
    }

    /**
     * What a long press on the power button opens. Veil uses it for the server.
     *
     * On the button rather than in a menu of its own: the engine and the thing
     * that configures the engine belong together, and it keeps one more control
     * off a bar that is meant to be almost empty.
     */
    protected fun setSettingsAction(onOpen: () -> Unit) {
        binding.power.setOnLongClickListener {
            onOpen()
            true
        }
    }

    // ---- browser -----------------------------------------------------------

    @SuppressLint("SetJavaScriptEnabled")
    private fun configureWebView() = with(binding.web) {
        // Debug builds only: lets the page be inspected from a desktop browser,
        // which is the difference between measuring what a site actually serves
        // and guessing at it. Never on in a release build.
        if (applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            WebView.setWebContentsDebuggingEnabled(true)
        }
        settings.javaScriptEnabled = true
        settings.domStorageEnabled = true
        settings.loadWithOverviewMode = true
        settings.useWideViewPort = true
        settings.builtInZoomControls = true
        settings.displayZoomControls = false
        // Video should start where the page says it starts, as in a real browser.
        settings.mediaPlaybackRequiresUserGesture = false

        // The page reports long presses on video through this.
        addJavascriptInterface(VideoHook(::onVideoLongPress), VideoHook.BRIDGE)
        addJavascriptInterface(YouTubeBridge(), YouTube.BRIDGE)

        // The YouTube recorder has to be in place before the page's own scripts
        // run. Injecting it when the page finishes loading was too late: the
        // player had already taken its own reference to `fetch`, so replacing
        // the global afterwards changed nothing and the request went unseen.
        recorderRunsAtStart = androidx.webkit.WebViewFeature.isFeatureSupported(
            androidx.webkit.WebViewFeature.DOCUMENT_START_SCRIPT
        )
        if (recorderRunsAtStart) {
            androidx.webkit.WebViewCompat.addDocumentStartJavaScript(
                this, YouTube.RECORDER, setOf("https://*.youtube.com", "https://youtube.com"),
            )
        }

        webViewClient = object : WebViewClient() {
            // Keep every navigation inside the app; handing one to Chrome would
            // send it out without the engine.
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) = false

            override fun onPageStarted(view: WebView, url: String, favicon: android.graphics.Bitmap?) {
                binding.url.setText(url)
                // Track which short is the reel's first, so only there does a pull reload.
                noteShortsUrl(url)
                // The star follows the page in front, and hides on the start page.
                updateStar(url)
                // A new page has no title yet; the chrome client fills it in.
                pageTitle = ""
                // Media found on the last page has nothing to do with this one.
                catcher.clear()
                // Media CDNs check the Referer; without this a download that
                // works in the player is refused outright.
                PageContext.current = PageContext(url, view.settings.userAgentString)
            }

            override fun onPageFinished(view: WebView, url: String) {
                // Whatever the reload was started by, this is where it ends.
                pull.finished()
                // One arrival, one entry — pushed onto the history where a page
                // finishes loading. Non-web addresses are filtered inside recordVisit.
                favorites.recordVisit(url, pageTitle)
                view.evaluateJavascript(VideoHook.SCRIPT, null)
                // Nudge responsive grids that laid out early and overlap (xvideos'
                // home thumbnails) to recompute — once now, once shortly after for
                // grids that fill in late. Parity with iOS/PC.
                view.evaluateJavascript(
                    "window.dispatchEvent(new Event('resize'));" +
                        "setTimeout(function(){window.dispatchEvent(new Event('resize'))},350);",
                    null,
                )
                // Only where the document-start injection is unavailable. Where
                // it works it has already run, and running the recorder again
                // would discard what it has captured since.
                if (!recorderRunsAtStart) view.evaluateJavascript(YouTube.RECORDER, null)
            }

            /**
             * Single-page sites navigate without loading a page.
             *
             * YouTube moves from its home screen to a video entirely in
             * JavaScript, so `onPageStarted` never fires — the address bar kept
             * showing the home URL, and everything keyed off it decided this
             * was not a video page. This is the callback that does fire.
             */
            override fun doUpdateVisitedHistory(view: WebView, url: String, isReload: Boolean) {
                if (url != binding.url.text.toString()) {
                    binding.url.setText(url)
                    // Shorts step between videos here (SPA), not through onPageStarted.
                    noteShortsUrl(url)
                    // In-app navigation (YouTube's SPA) changes the page too.
                    updateStar(url)
                    // A different video's streams are not this one's.
                    catcher.clear()
                    PageContext.current = PageContext(url, view.settings.userAgentString)
                    // Same document, different video: empty the recorder rather
                    // than let the last one's addresses answer for this one.
                    view.evaluateJavascript(YouTube.RECORDER, null)
                }
            }

            /**
             * Runs on a background thread for every request the page makes.
             * Returning null means "fetch it normally" — this only watches.
             */
            override fun shouldInterceptRequest(
                view: WebView,
                request: WebResourceRequest,
            ): WebResourceResponse? {
                val url = request.url?.toString() ?: return null
                // Block ad/tracker HOSTS at the network level, the same list iOS
                // blocks (WKContentRuleList) — this is what actually stops the ad,
                // where the DOM skipAds only clicks a skip button after it has
                // already started. Never the video CDN (googlevideo.com) or
                // youtube.com itself, so if a pattern ever stops matching, ads
                // return but nothing about YouTube breaks.
                if (isAdRequest(url)) {
                    return WebResourceResponse(
                        "text/plain", "utf-8", java.io.ByteArrayInputStream(ByteArray(0))
                    )
                }
                catcher.offer(url, pageTitle, request.requestHeaders ?: emptyMap())
                return null
            }
        }

        webChromeClient = object : WebChromeClient() {
            override fun onReceivedTitle(view: WebView, title: String?) {
                pageTitle = title.orEmpty()
            }

            /**
             * Open the phone's file picker when a page asks for a file.
             *
             * The page's own parameters are honoured — which kinds it accepts,
             * and whether it takes more than one — so a site asking for images
             * does not present the user with every document on the phone.
             */
            override fun onShowFileChooser(
                view: WebView,
                callback: android.webkit.ValueCallback<Array<Uri>>,
                params: FileChooserParams,
            ): Boolean {
                // Whatever was waiting before is answered with nothing, or it
                // would be left hanging for the life of the page.
                awaitingFiles?.onReceiveValue(null)
                awaitingFiles = callback
                val types = params.acceptTypes.filter { it.isNotBlank() }
                val intent = Intent(Intent.ACTION_GET_CONTENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = types.firstOrNull()?.takeIf { it.contains('/') } ?: "*/*"
                    if (types.size > 1) putExtra(Intent.EXTRA_MIME_TYPES, types.toTypedArray())
                    putExtra(
                        Intent.EXTRA_ALLOW_MULTIPLE,
                        params.mode == FileChooserParams.MODE_OPEN_MULTIPLE,
                    )
                }
                return runCatching {
                    filePicker.launch(Intent.createChooser(intent, params.title ?: ""))
                    true
                }.getOrElse {
                    awaitingFiles = null
                    callback.onReceiveValue(null)
                    false
                }
            }

            /** Page-side errors are otherwise invisible. */
            override fun onConsoleMessage(message: android.webkit.ConsoleMessage): Boolean {
                android.util.Log.d("shard-web", "${message.message()} @${message.lineNumber()}")
                return true
            }

            override fun onProgressChanged(view: WebView, newProgress: Int) {
                // A single-page site answers a reload without ever finishing a
                // page, so `onPageFinished` alone would leave the arrow turning.
                if (newProgress >= 100) pull.finished()
                binding.progress.progress = newProgress
                binding.progress.visibility =
                    if (newProgress in 1..99) View.VISIBLE else View.INVISIBLE
            }

            override fun onShowCustomView(view: View, callback: CustomViewCallback) {
                if (customView != null) {
                    callback.onCustomViewHidden()
                    return
                }
                customView = view
                customViewCallback = callback
                binding.fullscreen.addView(view)
                binding.fullscreen.visibility = View.VISIBLE
                binding.shell.visibility = View.GONE
            }

            override fun onHideCustomView() {
                val view = customView ?: return
                binding.fullscreen.removeView(view)
                binding.fullscreen.visibility = View.GONE
                binding.shell.visibility = View.VISIBLE
                customView = null
                customViewCallback?.onCustomViewHidden()
                customViewCallback = null
            }
        }

        // A page can also hand over a file directly rather than streaming it.
        setDownloadListener { url, _, _, _, size ->
            startDownload(Media(url, Media.Kind.FILE, pageTitle, emptyMap()), size)
        }
    }

    /**
     * Size the panel and park it off screen.
     *
     * A tenth of the screen, measured rather than fixed: the same dp value is a
     * third of a small phone and a sliver of a tablet.
     */
    private fun setUpPanel() {
        // A floor rather than a fixed height: the toolbar alone should not look
        // cramped, and the download rows should not be clipped by a number
        // chosen before they existed.
        binding.bar.minimumHeight = (resources.displayMetrics.heightPixels * PANEL_FRACTION).toInt()

        // Park it only once it has been measured. Doing this before layout
        // moves it by zero, which leaves it sitting open with the flag saying
        // it is closed — and then nothing appears to happen on a swipe.
        binding.bar.doOnLayout { hidePanel(animate = false) }

        // The swipe from the top-left is the only way in. Every visible
        // entrance tried here — a tap handle, a domain pill, a hint bar — sat
        // on the corner where pages put their own controls. The page wins.
    }

    // The shared motion ladder, the same one the library reads and the same
    // numbers the desktop shell keeps. A panel sliding is a "swap": longer than
    // a tint, shorter than a screen. It was 180ms on the platform's default
    // curve, which is a number nothing else in either app uses.
    private val tSwap by lazy { resources.getInteger(R.integer.t_swap).toLong() }
    private val easeEnter by lazy {
        android.view.animation.AnimationUtils.loadInterpolator(this, R.interpolator.ease_enter)
    }
    private val easeExit by lazy {
        android.view.animation.AnimationUtils.loadInterpolator(this, R.interpolator.ease_exit)
    }

    /** Slides in from the left edge, where the handle is. */
    /** Open the library over the page. The panel is browser furniture and has
     *  no business staying over a different screen, so it is put away first. */
    private fun openLibrary() {
        hidePanel()
        library.open()
    }

    private fun showPanel() {
        if (panelShown) return
        panelShown = true
        binding.bar.animate().translationX(0f)
            .setDuration(tSwap).setInterpolator(easeEnter).start()
    }

    private fun hidePanel(animate: Boolean = true) {
        panelShown = false
        dismissKeyboard()
        val offset = -binding.bar.width.toFloat()
        if (animate) {
            binding.bar.animate().translationX(offset)
                .setDuration(tSwap).setInterpolator(easeExit).start()
        } else {
            binding.bar.translationX = offset
        }
    }

    /**
     * Turn the browser a quarter, and pin it there.
     *
     * A page that only plays wide is watched sideways, but the auto-rotate
     * sensor turns with every shift of the hand and cannot be asked to hold. So
     * this is a switch, not a sensor: it toggles between the two fixed
     * orientations from whichever one is showing now.
     */
    private fun rotateScreen() {
        val portrait = resources.configuration.orientation ==
            android.content.res.Configuration.ORIENTATION_PORTRAIT
        requestedOrientation = if (portrait) {
            android.content.pm.ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        } else {
            android.content.pm.ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        }
        // Follow iOS: rotate.right while upright (a turn to landscape), rotate.left
        // while wide (a turn back). `portrait` here is the state BEFORE the turn.
        binding.rotate.setImageResource(
            if (portrait) R.drawable.ic_rotate_left else R.drawable.ic_rotate_right
        )
    }

    private fun wireControls() {
        binding.power.setOnClickListener { setEngine(!engineOn) }
        binding.rotate.setOnClickListener { rotateScreen() }

        // Home — the same two gestures iOS gives it. Tap goes to the set homepage;
        // a long press opens the editor to change it.
        binding.home.setOnClickListener {
            showFavorites(false)
            binding.web.loadUrl(favorites.homepage())
            hidePanel()
        }
        binding.home.setOnLongClickListener { editHomepage(); true }

        // Star — iOS's two gestures. Tap toggles the favorites page (an overlay);
        // a long press pins or unpins the page in front.
        binding.star.setOnClickListener { showFavorites(!favoritesShown) }
        binding.star.setOnLongClickListener {
            val url = binding.web.url ?: return@setOnLongClickListener true
            val added = favorites.toggle(url, pageTitle)
            updateStar(url)
            toast(if (added) "즐겨찾기에 추가했습니다" else "즐겨찾기에서 뺐습니다")
            true
        }

        // Tapping the address bar should replace what is there, not put a
        // cursor in the middle of a URL nobody wants to edit by hand.
        binding.url.setSelectAllOnFocus(true)

        binding.url.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_GO) {
                binding.web.loadUrl(asUrl(binding.url.text.toString()))
                dismissKeyboard()
                true
            } else {
                false
            }
        }
    }

    /**
     * A touch anywhere outside the address bar puts the keyboard away.
     *
     * Handled here rather than with a touch listener on the web view: a
     * listener would have to consume or forward every event and risks breaking
     * scrolling and long presses, while dispatch only observes.
     */
    /**
     * A sideways swipe near the top opens the panel.
     *
     * Run through a real detector rather than compared by hand. The earlier
     * version measured the deltas itself and missed almost every attempt: a
     * finger never travels in a straight line, and the first samples of a swipe
     * are indistinguishable from a tap.
     */
    private val swipeDetector by lazy {
        android.view.GestureDetector(this, object : android.view.GestureDetector.SimpleOnGestureListener() {
            override fun onScroll(
                down: MotionEvent?,
                current: MotionEvent,
                distanceX: Float,
                distanceY: Float,
            ): Boolean {
                val start = down ?: return false
                // The favorites overlay owns its own touches; leave it alone.
                if (favoritesShown) return false

                val dx = current.rawX - start.rawX
                val dy = current.rawY - start.rawY
                // Clearly more sideways than vertical, or a page that scrolls
                // would trip these every time.
                val sideways = kotlin.math.abs(dx) > swipeThreshold() &&
                    kotlin.math.abs(dx) > kotlin.math.abs(dy) * 1.2f
                if (!sideways) return false

                if (dx > 0) {
                    // Rightward from the left half opens the address panel.
                    if (!panelShown && startsInAddressBand(start.rawX, start.rawY)) {
                        showPanel()
                        return true
                    }
                } else {
                    // Leftward: if the address panel is open, the first swipe just
                    // puts it away; the next one (panel closed) opens the library —
                    // the same two-step iOS does. Otherwise open the library from
                    // the right half, but only over the front-most page.
                    if (panelShown) {
                        hidePanel()
                        return true
                    }
                    if (startsInLibraryBand(start.rawX, start.rawY) &&
                        customView == null && !(libraryMade && library.isOpen)) {
                        openLibrary()
                        return true
                    }
                }
                return false
            }

            override fun onSingleTapUp(e: MotionEvent): Boolean {
                // A tap outside the open panel closes it. Moved here from ACTION_DOWN:
                // a LEFT-swipe starts with a down outside the bar, and closing the
                // panel on that down cleared panelShown before onScroll could run its
                // "dismiss the panel first, library on the next swipe" step — so a
                // left-swipe on the right half jumped straight to the library.
                if (panelShown && !favoritesShown && !touchIsInside(binding.bar, e)) {
                    hidePanel()
                    return true
                }
                return false
            }
        })
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        if (this::binding.isInitialized) {
            swipeDetector.onTouchEvent(event)
            // The library reads the same stream to turn its shelf on a fling —
            // it cannot listen on its own views, because the list consumes its
            // touches for scrolling.
            if (libraryMade && library.isOpen) library.observeTouch(event)

            // Only while the page is the screen. The library covers it, the
            // panel is furniture over it, and full screen is a player — a drag
            // in any of those belongs to what is in front, not to the page.
            // On YouTube Shorts a vertical swipe steps between shorts, so the page owns it —
            // EXCEPT on the reel's first short, where a downward swipe has no previous short to
            // step to and should reload instead (user ask). So refresh is off mid-feed and on
            // for the first video; the swipe that steps back to a previous short never reloads.
            val sid = shortsIdOf(binding.web.url)
            val blockForShorts = sid != null && sid != shortsEntryId
            pull.enabled = !panelShown && customView == null &&
                !(libraryMade && library.isOpen) &&
                !blockForShorts
            pull.onTouch(event)

            when (event.action) {
                MotionEvent.ACTION_DOWN -> {
                    // Only the keyboard is put away on the down. Closing the panel is
                    // done on a single TAP (see onSingleTapUp) so a left-swipe is not
                    // pre-dismissed before its dismiss-then-library step can run.
                    if (binding.url.hasFocus() && !touchIsInside(binding.url, event)) {
                        dismissKeyboard()
                    }
                }
            }
        }
        return super.dispatchTouchEvent(event)
    }

    /**
     * Left half for the address panel: from well inside the edge (past the system
     * back-gesture strip) to the middle. The two bands used to be a left third and
     * a right third with a dead strip between them, which read as "only the centre
     * works"; splitting the width in half at the centre lets the two together cover
     * the whole screen — a rightward swipe anywhere on the left opens the address,
     * a leftward swipe anywhere on the right opens the library.
     */
    private fun startsInAddressBand(x: Float, y: Float): Boolean {
        val density = resources.displayMetrics.density
        val w = resources.displayMetrics.widthPixels
        return x > 48 * density && x < w * 0.5f
    }

    /**
     * Right half for the library: mirror of the address band, meeting it at the
     * centre so there is no dead zone between them.
     */
    private fun startsInLibraryBand(x: Float, y: Float): Boolean {
        val density = resources.displayMetrics.density
        val w = resources.displayMetrics.widthPixels
        return x < w - 48 * density && x >= w * 0.5f
    }

    private fun swipeThreshold() = 36 * resources.displayMetrics.density

    /**
     * Whether the touch landed on [view].
     *
     * Measured on the screen rather than with `getGlobalVisibleRect`, which
     * reports window coordinates: under edge-to-edge those differ from the raw
     * event coordinates by the status bar, and the mismatch made a tap on the
     * address bar read as a tap outside it — the keyboard appeared and was
     * dismissed in the same gesture.
     */
    private fun touchIsInside(view: View, event: MotionEvent): Boolean {
        val origin = IntArray(2)
        view.getLocationOnScreen(origin)
        val x = event.rawX.toInt()
        val y = event.rawY.toInt()
        return x >= origin[0] && x <= origin[0] + view.width &&
            y >= origin[1] && y <= origin[1] + view.height
    }

    private fun dismissKeyboard() {
        binding.url.clearFocus()
        WindowInsetsControllerCompat(window, binding.root).hide(WindowInsetsCompat.Type.ime())
        // Without somewhere else to put it, focus returns to the address bar
        // and the keyboard comes straight back.
        binding.web.requestFocus()
    }

    private fun handleBackPresses() {
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                when {
                    customView != null -> binding.web.webChromeClient?.onHideCustomView()
                    // The library is a screen over the browser, so it answers
                    // before anything belonging to the page does — and it
                    // answers twice: once to leave full screen, once to leave.
                    library.back() -> Unit
                    panelShown -> hidePanel()
                    binding.web.canGoBack() -> binding.web.goBack()
                    else -> finish()
                }
            }
        })
    }

    // ---- downloads ---------------------------------------------------------

    /**
     * Offer the choices and start whichever is picked.
     *
     * Shared by both routes into downloading — the general one that measures
     * candidates, and YouTube's, which is handed the list outright.
     */
    private fun showQualities(options: List<Qualities.Option>, fallbackTitle: String = "") {
        if (options.isEmpty()) {
            toast(getString(R.string.no_media))
            return
        }
        MaterialAlertDialogBuilder(this)
            .setTitle(R.string.quality_title)
            .setAdapter(qualityAdapter(options)) { _, which ->
                val chosen = options[which]
                // The page's name for this video, plus the chosen quality, so a
                // folder of downloads is readable at a glance.
                val source = listOf(chosen.pageTitle, fallbackTitle, pageTitle).firstOrNull { it.isNotBlank() }
                // A music file is named for the song, not the quality — "음악만
                // 저장" has no place in a filename. A video keeps its resolution
                // in the name so a folder of them reads at a glance.
                val name = if (chosen.audioOnly) {
                    source.orEmpty()
                } else {
                    listOfNotNull(source, chosen.title.takeIf { it.isNotBlank() }).joinToString(" ")
                }
                startDownload(
                    Media(
                        url = chosen.url,
                        kind = chosen.kind,
                        title = name,
                        headers = chosen.headers,
                        audioUrl = chosen.audioUrl,
                        expectedVideoBytes = chosen.videoBytes,
                        expectedAudioBytes = chosen.audioBytes,
                        sabr = chosen.sabr,
                        audioOnly = chosen.audioOnly,
                        cover = chosen.cover,
                    ),
                    chosen.size,
                )
            }
            .setNegativeButton(R.string.close, null)
            .show()
    }

    /**
     * Two lines per choice: what it is, then how big.
     *
     * A single line has to run "1080p · 2.8 Mbps · 340 MB · 스트리밍" together
     * and the eye cannot find the number that decides. Splitting it puts the
     * quality on one line and everything measured on the other.
     */
    private fun qualityAdapter(options: List<Qualities.Option>) =
        object : android.widget.ArrayAdapter<Qualities.Option>(this, R.layout.item_quality, options) {
            override fun getView(position: Int, convertView: View?, parent: android.view.ViewGroup): View {
                val view = convertView
                    ?: layoutInflater.inflate(R.layout.item_quality, parent, false)
                val option = options[position]

                view.findViewById<android.widget.TextView>(R.id.title).text = option.title
                view.findViewById<android.widget.TextView>(R.id.detail).text = option.detail
                view.findViewById<android.widget.ImageView>(R.id.icon).setImageResource(
                    if (option.audioOnly) R.drawable.ic_music else R.drawable.ic_video
                )
                // No line above the first row — a divider there reads as the
                // title being underlined rather than as a separator.
                view.findViewById<View>(R.id.divider).visibility =
                    if (position == 0) View.GONE else View.VISIBLE
                return view
            }
        }

    /** Called from the page's thread when a video was long-pressed. */
    private fun onVideoLongPress(target: VideoHook.VideoTarget) {
        ui.post {
            // The view's own URL, not the address bar: on a site that navigates
            // in JavaScript the bar can be a page behind.
            val here = binding.web.url ?: binding.url.text.toString()
            // YouTube states its own formats exactly, so nothing is inferred
            // there — and the answer arrives asynchronously from the page.
            if (YouTube.isWatchPage(here)) askYouTube() else offerQualities(target)
        }
    }

    private fun askYouTube() {
        if (choosing) return
        choosing = true
        showStatus(getString(R.string.looking_for_qualities))
        binding.web.evaluateJavascript(YouTube.SCRIPT, null)
    }

    /** The page's answer to [askYouTube]. */
    private inner class YouTubeBridge {
        @android.webkit.JavascriptInterface
        fun onFormats(payload: String) {
            val offer = YouTube.parse(payload)
            val reason = runCatching { org.json.JSONObject(payload).optString("reason") }.getOrDefault("")
            ui.post {
                choosing = false
                when {
                    offer.formats.isEmpty() -> toast(
                        if (reason == "no-player") getString(R.string.youtube_locked)
                        else getString(R.string.youtube_no_streams)
                    )
                    // The formats are known but the player has not fetched
                    // anything yet, so there is no request to copy. Playing for
                    // a second is all it takes, and saying that is more use than
                    // a list that cannot be acted on.
                    offer.notPlayedYet -> toast(getString(R.string.youtube_not_played))
                    else -> showQualities(
                        Qualities.forYouTube(offer, offer.title.ifBlank { pageTitle })
                    )
                }
            }
        }
    }

    /**
     * How the favorites overlay reaches the store: the two lists to draw, opening
     * a row, and the edits (remove one, clear the history). `home` is read on the
     * page's own thread and returns at once; the rest change the main web view or
     * the store, so they hop to the main thread. Opening a row navigates the MAIN
     * web view and closes the overlay — the overlay never becomes the page.
     */
    private inner class FavoritesBridge {
        @android.webkit.JavascriptInterface
        fun home(): String = favorites.homeJson()

        @android.webkit.JavascriptInterface
        fun open(url: String) {
            ui.post { showFavorites(false); binding.web.loadUrl(asUrl(url)); hidePanel() }
        }

        @android.webkit.JavascriptInterface
        fun removeBookmark(url: String) {
            favorites.removeBookmark(url)
            ui.post { binding.web.url?.let { updateStar(it) } }
        }

        @android.webkit.JavascriptInterface
        fun removeHistory(url: String) = favorites.removeHistory(url)

        @android.webkit.JavascriptInterface
        fun clearHistory() = favorites.clearHistory()
    }

    /**
     * Whether a request is to a known ad/tracker host or path — the same set iOS
     * blocks. Pure string matching so it is safe on the background request thread.
     * The video CDN (googlevideo.com) and youtube.com are deliberately absent.
     */
    private fun isAdRequest(url: String): Boolean {
        return AD_PATTERNS.any { url.contains(it) }
    }

    /**
     * The favorites overlay (start.html) — a second web view the star toggles over
     * the page, the way iOS overlays its start page. Configured once; only its
     * visibility changes after that.
     */
    private fun configureStartOverlay() {
        with(binding.startWeb) {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            addJavascriptInterface(FavoritesBridge(), Favorites.BRIDGE)
            loadUrl(Favorites.START_URL)
        }
    }

    /**
     * Show or hide the favorites overlay. Showing brings the address panel with it
     * (so home/star stay reachable) and re-renders the page so a just-bookmarked
     * site appears; hiding puts both away. Mirrors iOS's showStart.
     */
    private fun showFavorites(show: Boolean) {
        favoritesShown = show
        binding.startWeb.visibility = if (show) View.VISIBLE else View.GONE
        binding.star.setImageResource(
            when {
                show -> R.drawable.ic_star_accent
                binding.web.url?.let { favorites.isBookmarked(it) } == true -> R.drawable.ic_star_on
                else -> R.drawable.ic_star
            }
        )
        if (show) {
            binding.startWeb.evaluateJavascript("window.shardRender && window.shardRender()", null)
            showPanel()
        } else {
            hidePanel()
        }
    }

    /** A small dialog to set the homepage, the same as iOS's homepage editor. */
    private fun editHomepage() {
        val field = EditText(this).apply {
            setText(favorites.homepage())
            setSelection(text.length)
            inputType = android.text.InputType.TYPE_TEXT_VARIATION_URI
            setSingleLine()
        }
        android.app.AlertDialog.Builder(this)
            .setTitle("홈페이지")
            .setMessage("홈 버튼을 누르면 이동할 주소")
            .setView(field)
            .setPositiveButton("저장") { _, _ -> favorites.setHomepage(field.text.toString()) }
            .setNegativeButton("취소", null)
            .show()
    }

    /**
     * Fill or empty the address-bar star for the page in front. Filled while the
     * page is bookmarked; accent while the favorites overlay is up (iOS's colours).
     */
    private fun updateStar(url: String) {
        if (favoritesShown) {
            binding.star.setImageResource(R.drawable.ic_star_accent)
            return
        }
        binding.star.setImageResource(
            if (favorites.isBookmarked(url)) R.drawable.ic_star_on else R.drawable.ic_star
        )
    }

    /**
     * Work out what can be downloaded, then let the user pick.
     *
     * Resolving means fetching a playlist, so it happens off the main thread —
     * and the button is latched meanwhile, because a long press that produces
     * nothing for a second invites a second press.
     */
    private fun offerQualities(target: VideoHook.VideoTarget) {
        if (choosing) return
        choosing = true
        showStatus(getString(R.string.looking_for_qualities))

        lifecycleScope.launch {
            val found = withContext(Dispatchers.IO) {
                runCatching { Qualities.forTarget(target, catcher.items) }
            }
            choosing = false

            // Reporting the reason beats "no video here", which is what a
            // swallowed exception looked like and told nobody anything.
            val options = found.getOrElse {
                toast(getString(R.string.download_failed, it.message ?: ""))
                return@launch
            }
            if (options.isEmpty()) {
                toast(getString(R.string.no_media))
                return@launch
            }
            showQualities(options, target.title)
        }
    }

    /**
     * Start a download and give it a gauge.
     *
     * One gauge per download rather than one shared line: several can run at
     * once, and a status line that flickers between them tells you about none
     * of them. The gauge is removed a moment after it fills, so a finished
     * download is seen to finish rather than vanishing mid-progress.
     */
    /**
     * Hand the download to the service.
     *
     * Not run here: the work outlives this screen, and a download that dies
     * because the user checked a message is worse than no download feature.
     * The service also keeps the process — and so the engine the download is
     * going through — alive.
     */
    private fun startDownload(media: Media, expectedBytes: Long) {
        // Already on a shelf, from some other day. Asked rather than refused —
        // a file may be worth having again, cut short the first time or saved
        // before something in here was fixed — and asked every time, because a
        // warning that stops warning is one nobody can rely on.
        val title = Storage.sanitise(media.title.trim()).trimEnd('.')
        if (title.isNotBlank()) {
            lifecycleScope.launch {
                val saved = withContext(Dispatchers.IO) {
                    Library.items(applicationContext).any { it.title == title }
                }
                if (saved) askToDownloadAgain(media, expectedBytes) else hand(media, expectedBytes)
            }
            return
        }
        hand(media, expectedBytes)
    }

    private fun askToDownloadAgain(media: Media, expectedBytes: Long) {
        MaterialAlertDialogBuilder(this)
            .setMessage(getString(R.string.download_already_saved))
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.download_again) { _, _ -> hand(media, expectedBytes) }
            .show()
    }

    private fun hand(media: Media, expectedBytes: Long) {
        DownloadService.enqueue(applicationContext, media, expectedBytes)
        showPanel()
        toast(getString(R.string.downloading))
    }

    /**
     * Redraw the download rows.
     *
     * The whole list is rebuilt each second rather than diffed: there are never
     * more than a handful, and matching views to jobs is more code than it is
     * worth for something that changes every tick anyway.
     */
    private fun refreshDownloads() {
        val jobs = Downloads.snapshot
        binding.downloadDivider.visibility = if (jobs.isEmpty()) View.GONE else View.VISIBLE

        if (jobs.isEmpty()) {
            binding.downloads.removeAllViews()
            return
        }
        while (binding.downloads.childCount > jobs.size) {
            binding.downloads.removeViewAt(binding.downloads.childCount - 1)
        }
        while (binding.downloads.childCount < jobs.size) {
            binding.downloads.addView(
                layoutInflater.inflate(R.layout.item_download, binding.downloads, false)
            )
        }

        jobs.forEachIndexed { index, job ->
            val row = binding.downloads.getChildAt(index)
            row.findViewById<android.widget.TextView>(R.id.name).text = job.name

            // Only what is still running can be stopped; a finished row is
            // about to disappear on its own.
            val stop = row.findViewById<android.widget.ImageButton>(R.id.stop)
            stop.visibility =
                if (job.state == Downloads.State.RUNNING) View.VISIBLE else View.INVISIBLE
            stop.setOnClickListener {
                Downloads.cancel(job)
                toast(getString(R.string.download_cancelled))
                refreshDownloads()
            }

            val bar = row.findViewById<android.widget.ProgressBar>(R.id.gauge)
            val percent = job.percent
            bar.isIndeterminate = percent == null && job.state == Downloads.State.RUNNING
            if (percent != null) bar.progress = percent

            row.findViewById<android.widget.TextView>(R.id.percent).text = when {
                job.state == Downloads.State.DONE -> getString(R.string.download_done)
                job.state == Downloads.State.FAILED -> getString(R.string.download_failed_short)
                job.joining -> getString(R.string.download_joining)
                else -> percent?.let { "$it%" } ?: ""
            }

            row.findViewById<android.widget.TextView>(R.id.detail).text = when (job.state) {
                Downloads.State.FAILED -> job.error
                else -> listOf(
                    if (job.total > 0) "${Downloads.bytes(job.done)} / ${Downloads.bytes(job.total)}"
                    else Downloads.bytes(job.done),
                    Downloads.rate(job.rate),
                    Downloads.remaining(job.secondsLeft),
                ).joinToString("  ·  ")
            }
        }
    }

    // ---- odds and ends -----------------------------------------------------

    protected fun toast(text: String) = Toast.makeText(this, text, Toast.LENGTH_SHORT).show()

    companion object {
        /**
         * Ad/tracker host and path fragments to block — the same set iOS blocks
         * (see WebView.installAdBlock). The video CDN (googlevideo.com) and
         * youtube.com are NOT here on purpose: block those and playback breaks.
         */
        private val AD_PATTERNS = listOf(
            "doubleclick.net",
            "googlesyndication.com",
            "googleadservices.com",
            "google-analytics.com",
            "googletagservices.com",
            "googletagmanager.com",
            "/pagead/",
            "/ptracking",
            "/api/stats/ads",
        )

        /** Panel height as a share of the screen. */
        private const val PANEL_FRACTION = 0.10

        /** How long the last rate stays on screen once traffic stops. */
        private const val RATE_HOLD_MS = 4000L

        /** How long a finished gauge stays up, so the end is visible. */
        private const val GAUGE_LINGER_MS = 2500L

        /** Human-readable byte count for the status line. */
        fun formatBytes(bytes: Long): String {
            val units = listOf("GB" to (1L shl 30), "MB" to (1L shl 20), "KB" to (1L shl 10))
            for ((unit, scale) in units) {
                if (bytes >= scale) return String.format("%.1f %s", bytes.toDouble() / scale, unit)
            }
            return "$bytes B"
        }

        /**
         * Whether what was typed is meant as an address rather than as words to
         * search for: a dot and no space.
         *
         * Split out from [asUrl] so it can be tested. The decision is the part
         * that can be got wrong; the rest is a string join and a call into
         * `Uri`, which does not exist off a phone and so takes the whole
         * function out of reach of a plain unit test.
         */
        fun looksLikeAddress(input: String): Boolean {
            val text = input.trim()
            return !text.contains(' ') && text.contains('.')
        }

        /**
         * What to load for what was typed.
         *
         * Search goes to Google. That is a different question from where the
         * browser opens — see `homeUrl`, which is YouTube — and the two were
         * once one value.
         */
        fun asUrl(input: String): String {
            val text = input.trim()
            return when {
                text.startsWith("http://") || text.startsWith("https://") -> text
                looksLikeAddress(text) -> "https://$text"
                else -> "https://www.google.com/search?q=" + android.net.Uri.encode(text)
            }
        }
    }
}
