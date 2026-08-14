package net.sw.browser

import androidx.webkit.ProxyConfig
import androidx.webkit.ProxyController
import androidx.webkit.WebViewFeature
import java.net.InetSocketAddress

/**
 * Points this app's web views at whichever local engine is running.
 *
 * This is the whole reason neither Android build needs a VPN permission. A
 * `VpnService` would capture every app's packets and asks the user to approve a
 * system-wide tunnel; a proxy override applies to this app's web views and
 * nothing else — which is exactly the scope that was wanted. Shard puts its
 * desync engine behind this, Veil puts a tunnel or Tor behind it, and the
 * browser above does not know or care which.
 */
object ProxyRoute {

    enum class Kind {
        /** A local HTTP proxy — the desync engine, or the tunnel client. */
        HTTP,

        /**
         * SOCKS5 with the name resolved at the far end.
         *
         * Required for Tor: resolving locally would send every hostname to the
         * ISP's resolver in the clear, which is exactly what the tunnel exists
         * to prevent, and would defeat onion addresses entirely.
         */
        SOCKS,
    }

    val supported: Boolean
        get() = WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)

    /** Where the engine is listening, once started. Null when off. */
    @Volatile
    var address: InetSocketAddress? = null
        private set

    @Volatile
    var kind: Kind = Kind.HTTP
        private set

    /** Route web views through 127.0.0.1:[port]. */
    fun enable(port: Int, kind: Kind = Kind.HTTP, onApplied: () -> Unit) {
        if (!supported) return
        val rule = when (kind) {
            Kind.HTTP -> "127.0.0.1:$port"
            Kind.SOCKS -> "socks5://127.0.0.1:$port"
        }
        val config = ProxyConfig.Builder()
            // No bypass list: a rule that let some hosts go direct would leak
            // exactly the requests worth protecting.
            .addProxyRule(rule)
            .build()
        address = InetSocketAddress("127.0.0.1", port)
        this.kind = kind
        ProxyController.getInstance().setProxyOverride(config, { it.run() }) { onApplied() }
    }

    /** Send web views straight out again. */
    fun disable(onApplied: () -> Unit) {
        address = null
        if (!supported) return
        ProxyController.getInstance().clearProxyOverride({ it.run() }) { onApplied() }
    }

    /** A [java.net.Proxy] for the downloader, so files take the same path. */
    fun forDownloads(): java.net.Proxy {
        val at = address ?: return java.net.Proxy.NO_PROXY
        val type = when (kind) {
            Kind.HTTP -> java.net.Proxy.Type.HTTP
            Kind.SOCKS -> java.net.Proxy.Type.SOCKS
        }
        return java.net.Proxy(type, at)
    }
}
