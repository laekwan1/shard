package net.sw.browser

import android.content.ContentValues
import android.content.Context
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import java.io.File
import java.io.OutputStream

/**
 * Where a finished download lands.
 *
 * From Android 10 the gallery is reached through MediaStore and needs no
 * permission; before that it was a plain path plus a storage permission. Both
 * are handled so the file ends up somewhere the user can actually find it,
 * rather than in app-private storage that a file manager will not show.
 */
object Storage {

    class Sink(val stream: OutputStream, private val finish: () -> Unit, val where: String) :
        AutoCloseable {
        override fun close() {
            stream.close()
            finish()
        }
    }

    fun create(context: Context, name: String, mime: String): Sink {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) mediaStore(context, name, mime)
        else legacy(name)
    }

    /**
     * The same destination, opened as a file rather than a stream.
     *
     * The muxer writes through a descriptor and seeks around inside it — a
     * container's index is finished after its payload. Handing it the real
     * destination is what lets a download land where it belongs the moment the
     * last frame is written, instead of being built in the cache and then
     * copied across in full. On a large file that copy was most of the wait
     * after the progress bar reached the end.
     */
    class FileSink(
        val descriptor: android.os.ParcelFileDescriptor,
        private val finish: () -> Unit,
        val where: String,
        /**
         * The exact row this file is, for undoing it.
         *
         * The media store may rename a file to avoid a clash — a second copy of
         * "song.m4a" becomes "song (1).m4a" — so deleting later by the name that
         * was asked for could match the wrong file and take the earlier, good
         * one instead. The row's own address never moves, so a failure removes
         * precisely what it wrote and nothing else. Absent on the older versions
         * that write a plain path, where [where] is the file itself.
         */
        val uri: android.net.Uri? = null,
    ) : AutoCloseable {
        override fun close() {
            descriptor.close()
            finish()
        }
    }

    fun createFile(context: Context, name: String, mime: String): FileSink {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            val dir = File(
                Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MOVIES),
                FOLDER,
            )
            dir.mkdirs()
            val file = File(dir, name)
            val mode = android.os.ParcelFileDescriptor.MODE_READ_WRITE or
                android.os.ParcelFileDescriptor.MODE_CREATE or
                android.os.ParcelFileDescriptor.MODE_TRUNCATE
            return FileSink(
                android.os.ParcelFileDescriptor.open(file, mode),
                {},
                file.absolutePath,
            )
        }

        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, mime)
            put(MediaStore.MediaColumns.RELATIVE_PATH, Library.relativePath(""))
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val collection = MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val uri = resolver.insert(collection, values)
            ?: throw IllegalStateException("저장 위치를 만들 수 없습니다")
        // "rw" rather than "w": the muxer needs to seek back.
        val descriptor = resolver.openFileDescriptor(uri, "rw")
            ?: throw IllegalStateException("저장 파일을 열 수 없습니다")

        return FileSink(descriptor, {
            resolver.update(uri, ContentValues().apply {
                put(MediaStore.MediaColumns.IS_PENDING, 0)
            }, null, null)
        }, "${Library.MOVIES}/$FOLDER/$name", uri)
    }

    /**
     * A music file, in the phone's own Music folder.
     *
     * The Music folder is a standard one every phone has, so from Android 10 the
     * media store writes there directly and a music app finds it beside
     * everything else. Only where that is not available — the older versions
     * before the media store took over — does it fall back to a Music folder of
     * the app's own under Downloads.
     */
    fun createMusicFile(context: Context, name: String, mime: String): FileSink {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            val music = File(
                Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MUSIC),
                FOLDER,
            )
            val dir = if (music.isDirectory || music.mkdirs()) music
            else File(
                Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS),
                "$FOLDER/Music",
            ).apply { mkdirs() }
            val file = File(dir, name)
            val mode = android.os.ParcelFileDescriptor.MODE_READ_WRITE or
                android.os.ParcelFileDescriptor.MODE_CREATE or
                android.os.ParcelFileDescriptor.MODE_TRUNCATE
            return FileSink(android.os.ParcelFileDescriptor.open(file, mode), {}, file.absolutePath)
        }

        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, mime)
            put(MediaStore.MediaColumns.RELATIVE_PATH, Library.musicPath(""))
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val collection = MediaStore.Audio.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val uri = resolver.insert(collection, values)
            ?: throw IllegalStateException("저장 위치를 만들 수 없습니다")
        val descriptor = resolver.openFileDescriptor(uri, "rw")
            ?: throw IllegalStateException("저장 파일을 열 수 없습니다")
        return FileSink(descriptor, {
            resolver.update(uri, ContentValues().apply {
                put(MediaStore.MediaColumns.IS_PENDING, 0)
            }, null, null)
        }, "${Environment.DIRECTORY_MUSIC}/$FOLDER/$name", uri)
    }

    /**
     * Videos land in `Movies/Shard`.
     *
     * A folder directly beside `Movies` — `/sdcard/Shard` — is not something an
     * app can create: since Android 10 the only writable top-level names are the
     * standard ones, and anything else needs the all-files permission that
     * grants access to every document on the phone. `Movies/Shard` is the
     * standard place a video belongs, where the gallery and video apps look, and
     * it costs no permission at all.
     */
    private const val FOLDER = Library.ROOT

    /**
     * The Video collection, which is the one that owns `Movies`.
     *
     * Each MediaStore collection only accepts certain top-level folders: the
     * video collection allows Movies, DCIM and Pictures — and refuses `Download`
     * outright — so a file kept under `Movies` is written to the collection that
     * owns it. (Music goes the same way through its own audio collection.)
     */
    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun mediaStore(context: Context, name: String, mime: String): Sink {
        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, mime)
            put(MediaStore.MediaColumns.RELATIVE_PATH, Library.relativePath(""))
            // Hidden from pickers until the bytes are all there, so a
            // half-written file is never offered for playback.
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val collection = MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val uri = resolver.insert(collection, values)
            ?: throw IllegalStateException("저장 위치를 만들 수 없습니다")
        val stream = resolver.openOutputStream(uri)
            ?: throw IllegalStateException("저장 파일을 열 수 없습니다")

        return Sink(stream, {
            resolver.update(uri, ContentValues().apply {
                put(MediaStore.MediaColumns.IS_PENDING, 0)
            }, null, null)
        }, "${Library.MOVIES}/$FOLDER/$name")
    }

    private fun legacy(name: String): Sink {
        val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MOVIES), FOLDER)
        dir.mkdirs()
        val file = File(dir, name)
        return Sink(file.outputStream(), {}, file.absolutePath)
    }

    /**
     * Delete a half-written file this module created, by its exact address.
     *
     * The row's own [uri][FileSink.uri] is deleted, not a name lookup: a name
     * could have been changed to dodge a clash and then match an earlier, good
     * file of the same title instead of the partial. On the older versions with
     * no media store, [where][FileSink.where] is the path and it is removed
     * directly.
     *
     * Best effort: a leftover on a failure is untidy, and failing to tidy it is
     * not worth reporting over whatever went wrong first.
     */
    fun remove(context: Context, sink: FileSink) {
        runCatching {
            val uri = sink.uri
            if (uri != null) context.contentResolver.delete(uri, null, null)
            else File(sink.where).delete()
        }
    }

    /** Strip what a filesystem will not take, and keep the length sane. */
    fun sanitise(name: String): String =
        name.replace(Regex("[\\\\/:*?\"<>|\\r\\n]"), " ").trim().take(80).ifBlank { "video" }

    /** A filesystem-safe name derived from a page title. */
    fun nameFrom(title: String, extension: String): String {
        // A title ending in a full stop would otherwise produce "name..mp4".
        val base = sanitise(title).trimEnd('.').ifBlank { "video" }
        return "$base.$extension"
    }
}
