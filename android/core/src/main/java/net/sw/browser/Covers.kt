package net.sw.browser

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import java.io.File

/**
 * Cover art for songs, kept in the app's own storage.
 *
 * A music-only download is a bare .m4a with no picture, so the video's
 * thumbnail is fetched at download time and remembered here, keyed by the song's
 * name. The library reads it back for the tile when the file itself carries no
 * embedded art. Private to the app — it is only a nicety for this screen, not
 * something other apps need — and small, so it is a cache, not a store: a phone
 * short on space may clear it, and the worst that costs is a blank tile.
 */
object Covers {

    private fun dir(context: Context) = File(context.filesDir, "covers").apply { mkdirs() }

    /** Hashed so any title — Korean, emoji, punctuation — makes a safe filename. */
    private fun file(context: Context, key: String) =
        File(dir(context), Integer.toHexString(key.hashCode()) + ".jpg")

    /** The key a song's cover is filed under: its name without the extension. */
    fun keyFor(name: String) = name.substringBeforeLast('.', name)

    fun save(context: Context, key: String, bytes: ByteArray) {
        runCatching {
            // Decoded and re-encoded rather than stored raw: it caps the size a
            // huge source thumbnail would otherwise take, and a tile never needs
            // more than a few hundred pixels — the phone shows the cover only in
            // the list now, never blown up to the whole screen.
            val full = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return
            val side = 320
            val scale = minOf(1f, side.toFloat() / maxOf(full.width, full.height))
            val small = if (scale < 1f) {
                Bitmap.createScaledBitmap(full, (full.width * scale).toInt(), (full.height * scale).toInt(), true)
            } else {
                full
            }
            File(dir(context), Integer.toHexString(key.hashCode()) + ".jpg").outputStream().use {
                small.compress(Bitmap.CompressFormat.JPEG, 85, it)
            }
        }
    }

    fun load(context: Context, key: String): Bitmap? = runCatching {
        val f = file(context, key)
        if (f.exists()) BitmapFactory.decodeFile(f.path) else null
    }.getOrNull()
}
