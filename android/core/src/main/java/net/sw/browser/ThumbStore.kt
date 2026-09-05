package net.sw.browser

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import java.io.File

/**
 * Decoded list thumbnails, kept on disk so a fresh app launch fills the library
 * at once instead of small.
 *
 * The in-memory LruCache dies with the process, so every time the app is started
 * again the first library open had to re-decode every frame — and a 4K frame
 * takes long enough that each row showed its small placeholder icon until it
 * landed, then filled full-size only after the cache warmed, which is why leaving
 * the folder and coming back "fixed" it. Remembering the frame on disk means the
 * next launch reads a ready 320px JPEG instead of opening the whole video again.
 *
 * A cache, not a store: it lives in cacheDir because every frame can be decoded
 * again from the file it came from, so the OS may clear it under space pressure
 * and the worst that costs is one more decode. Keyed by id+size+time so replacing
 * a file behind the same MediaStore id gives a fresh key, never a stale frame.
 */
object ThumbStore {

    private fun dir(context: Context) = File(context.cacheDir, "thumbs").apply { mkdirs() }

    /** Hashed so the key — a long uri or name — makes a safe, short filename. */
    private fun file(context: Context, key: String) =
        File(dir(context), Integer.toHexString(key.hashCode()) + ".jpg")

    /**
     * What a file's thumbnail is filed under. Size and time ride along so a
     * download that replaces an older file at the same store id — same id, new
     * bytes — misses the old thumbnail instead of showing the wrong picture.
     */
    fun keyFor(item: Library.Item) = "${item.id}_${item.bytes}_${item.addedAt}"

    fun load(context: Context, key: String): Bitmap? = runCatching {
        val f = file(context, key)
        if (f.exists()) BitmapFactory.decodeFile(f.path) else null
    }.getOrNull()

    fun save(context: Context, key: String, bmp: Bitmap) {
        // The frame is already scaled to <=320 by the decode; store it as it is.
        // JPEG 85 keeps a tile-sized file tiny and a 68dp tile never shows the loss.
        runCatching {
            file(context, key).outputStream().use { bmp.compress(Bitmap.CompressFormat.JPEG, 85, it) }
        }
    }
}
