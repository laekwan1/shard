package net.sw.browser

import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import java.io.File

/**
 * What has been saved, and what can be done with it.
 *
 * Videos land in `Movies/Shard` and music in `Music/Shard`, where the gallery
 * and music apps can see them and any player can open them — that is the point
 * of putting them there rather than in the app's own storage. So the library
 * does not keep a database of its own: it asks the system what is in those
 * folders every time it is opened. A file deleted from a file manager is then
 * simply gone from here too, rather than being a row that opens nothing.
 *
 * Folders are real directories underneath it. Grouping is therefore something
 * the phone keeps, not something only this app understands — move a file out
 * with a file manager and the grouping moves with it.
 */
object Library {

    /** The app's own corner of each public folder: `Movies/Shard`, `Music/Shard`. */
    const val ROOT = "Shard"

    /**
     * The public downloads folder, by name.
     *
     * Spelled out rather than read from `Environment.DIRECTORY_DOWNLOADS`. The
     * value is the same — it is fixed by the platform and is the string the
     * media store keeps in every row — but the framework field is not a
     * compile-time constant, so off a phone it reads as null and every path
     * built from it comes out wrong in a way nothing complains about.
     */
    const val DOWNLOADS = "Download"

    /**
     * The public movies folder, by name, for the same reason as [DOWNLOADS].
     *
     * Videos are saved under `Movies/Shard` — inside the folder the gallery and
     * every video app already watch, so a download shows up beside the phone's
     * own videos, and in a corner of it that is only ours, so the library lists
     * what this app saved and not the phone's whole camera roll.
     */
    const val MOVIES = "Movies"

    /**
     * The public music folder, by name, for the same reason as [DOWNLOADS].
     *
     * Music of its own is saved under `Music/Shard` — inside the folder every
     * music app already watches, so it is found beside everything else, and in
     * a corner of it that is only ours, so the library can list what this app
     * saved without dragging in the phone's whole music collection.
     */
    const val MUSIC = "Music"

    /** Whether a saved file is a video or a music-only track. */
    enum class Kind { VIDEO, MUSIC }

    /** One saved file. */
    data class Item(
        val id: Long,
        val uri: Uri,
        val name: String,
        /** The folder it sits in, or "" for the top of the library. */
        val folder: String,
        val bytes: Long,
        /** When it was saved, in seconds since the epoch. */
        val addedAt: Long,
        /** Video or music — what shelf it belongs on. */
        val kind: Kind = Kind.VIDEO,
    ) {
        /** The name without its extension, which is what a title should read as. */
        val title: String get() = name.substringBeforeLast('.', name)
    }

    // ---- reading -----------------------------------------------------------

    /**
     * Everything saved, newest first.
     *
     * Blocking: it queries the media store. Call it off the main thread, the
     * same as anything else that touches storage.
     */
    fun items(context: Context): List<Item> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            (fromMediaStore(context) + musicFromMediaStore(context))
                .sortedByDescending { it.addedAt }
        } else {
            fromDisk()
        }

    /**
     * The folders in use, plus any that were made and are still empty.
     *
     * An empty folder does not exist on disk — a directory with nothing in it is
     * not something the media store will report — so the names of folders made
     * but not yet filled are remembered separately. They stop being remembered
     * once something is in them, because by then the folder speaks for itself.
     */
    fun folders(context: Context, items: List<Item>, kind: Kind): List<String> {
        val used = items.filter { it.kind == kind && it.folder.isNotBlank() }.map { it.folder }.toSet()
        return (used + remembered(context, kind)).sorted()
    }

    /** Remember a folder name so it can be shown before anything is in it. */
    fun addFolder(context: Context, name: String, kind: Kind) {
        val clean = clean(name)
        if (clean.isBlank()) return
        prefs(context).edit().putStringSet(emptyKey(kind), remembered(context, kind) + clean).apply()
    }

    /**
     * Forget a folder, moving anything inside it back to the top.
     *
     * Deleting the files with it would be the other reading of "remove this
     * folder", and it is the one that cannot be undone — so it is not the one
     * this does.
     */
    fun removeFolder(context: Context, name: String, items: List<Item>, kind: Kind) {
        items.filter { it.kind == kind && it.folder == name }.forEach { move(context, it, "") }
        prefs(context).edit().putStringSet(emptyKey(kind), remembered(context, kind) - name).apply()
    }

    // ---- changing ----------------------------------------------------------

    /**
     * Put an item in a folder, or back at the top when [folder] is blank.
     *
     * Returns false when the phone would not do it, which happens on the
     * versions where a file's location is not something an app may change.
     */
    fun move(context: Context, item: Item, folder: String): Boolean {
        val clean = clean(folder)
        if (item.folder == clean) return true
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val values = ContentValues().apply {
                put(MediaStore.MediaColumns.RELATIVE_PATH, pathFor(item.kind, clean))
            }
            runCatching { context.contentResolver.update(item.uri, values, null, null) > 0 }
                .getOrDefault(false)
        } else {
            val root = if (item.kind == Kind.MUSIC) musicLegacyRoot() else legacyRoot()
            val target = File(root, clean).apply { mkdirs() }
            val from = File(item.uri.path ?: return false)
            runCatching { from.renameTo(File(target, item.name)) }.getOrDefault(false)
        }
    }

    /** Remove a saved file for good. */
    fun delete(context: Context, item: Item): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            runCatching { context.contentResolver.delete(item.uri, null, null) > 0 }
                .getOrDefault(false)
        } else {
            runCatching { File(item.uri.path ?: return false).delete() }.getOrDefault(false)
        }

    // ---- naming ------------------------------------------------------------

    /**
     * A folder name the file system will accept.
     *
     * Separators are the ones that matter: a name carrying one would put the
     * folder somewhere else entirely, which is not what typing it meant.
     */
    fun clean(name: String): String =
        name.trim().replace(Regex("""[\\/:*?"<>|]"""), "").take(40)

    /** Where a folder sits, as the media store spells it: under `Movies/Shard`. */
    fun relativePath(folder: String): String {
        val base = "$MOVIES/$ROOT"
        return if (folder.isBlank()) "$base/" else "$base/$folder/"
    }

    /** Where music sits, as the media store spells it: under `Music/Shard`. */
    fun musicPath(folder: String): String {
        val base = "$MUSIC/$ROOT"
        return if (folder.isBlank()) "$base/" else "$base/$folder/"
    }

    /** Where a folder sits for a given shelf: videos under Movies, music under Music. */
    fun pathFor(kind: Kind, folder: String): String =
        if (kind == Kind.MUSIC) musicPath(folder) else relativePath(folder)

    /**
     * The folder out of a stored path.
     *
     * The media store reports where a file is as a trailing path — the same
     * string that was written when it was put there. Everything after the
     * library's own root, and before the file, is the folder; nothing there
     * means the top of the library.
     */
    fun folderOf(relativePath: String): String {
        return folderIn(relativePath, Kind.VIDEO)
    }

    /** The folder out of a stored path, for the shelf it belongs to. */
    private fun folderIn(relativePath: String, kind: Kind): String {
        val marker = if (kind == Kind.MUSIC) "$MUSIC/$ROOT" else "$MOVIES/$ROOT"
        val at = relativePath.indexOf(marker)
        if (at < 0) return ""
        return relativePath.substring(at + marker.length).trim('/')
    }

    // ---- where it is kept --------------------------------------------------

    private const val PREFS = "shard-library"
    private const val EMPTY_FOLDERS = "empty-folders"
    private const val EMPTY_FOLDERS_MUSIC = "empty-folders-music"
    private const val BACKGROUND = "background-playback"
    // The two booleans an earlier build used, kept only so a setting already
    // made can be migrated to [END_ACTION] once.
    private const val AUTOPLAY = "autoplay-next"
    private const val SHUFFLE = "shuffle-play"
    private const val END_ACTION = "playback-end"

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /**
     * Whether sound carries on when the app is put away.
     *
     * Off by default. A video that keeps playing after the phone is put in a
     * pocket is a surprise, and a surprise that costs battery — so it is a
     * choice rather than a behaviour, and the choice is remembered.
     */
    fun backgroundPlayback(context: Context): Boolean =
        prefs(context).getBoolean(BACKGROUND, false)

    fun setBackgroundPlayback(context: Context, on: Boolean) {
        prefs(context).edit().putBoolean(BACKGROUND, on).apply()
    }

    /**
     * What happens when a video ends.
     *
     * One choice rather than two switches. "Keep playing" and "play at random"
     * are not two independent things to turn on — the first is *whether* to go
     * on, the second is *in what order*, and a checkbox each leaves the nonsense
     * state of "random order, but do not go on". A single choice of what the end
     * of a video means has exactly one answer and no nonsense state.
     */
    enum class PlaybackEnd { STOP, NEXT, SHUFFLE }

    /**
     * The chosen end action, defaulting to stop.
     *
     * Migrated once from the two booleans an earlier build stored, so a setting
     * already made carries over: shuffle wins, then next, then stop.
     */
    fun playbackEnd(context: Context): PlaybackEnd {
        val stored = prefs(context).getString(END_ACTION, null)
        if (stored != null) {
            return runCatching { PlaybackEnd.valueOf(stored) }.getOrDefault(PlaybackEnd.STOP)
        }
        val migrated = when {
            prefs(context).getBoolean(SHUFFLE, false) -> PlaybackEnd.SHUFFLE
            prefs(context).getBoolean(AUTOPLAY, false) -> PlaybackEnd.NEXT
            else -> PlaybackEnd.STOP
        }
        setPlaybackEnd(context, migrated)
        return migrated
    }

    fun setPlaybackEnd(context: Context, value: PlaybackEnd) {
        prefs(context).edit().putString(END_ACTION, value.name).apply()
    }

    private fun remembered(context: Context, kind: Kind): Set<String> =
        prefs(context).getStringSet(emptyKey(kind), emptySet()) ?: emptySet()

    private fun emptyKey(kind: Kind) =
        if (kind == Kind.MUSIC) EMPTY_FOLDERS_MUSIC else EMPTY_FOLDERS

    private fun legacyRoot() =
        File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MOVIES), ROOT)

    private fun musicLegacyRoot() =
        File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MUSIC), ROOT)

    /** The videos, from the video collection under `Movies/Shard`. */
    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun fromMediaStore(context: Context): List<Item> {
        // Everything under the library's root, at any depth. The trailing
        // wildcard is what picks up the folders inside it.
        return queryStore(
            context,
            MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY),
            "$MOVIES/$ROOT/%",
            Kind.VIDEO,
        )
    }

    /**
     * The music, from the audio collection under `Music/Shard`.
     *
     * Only this app's own tracks come back even without a media permission: an
     * app can always read the media it created, and the query is narrowed to
     * our own corner of Music besides — so the phone's other music never shows
     * up here.
     */
    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun musicFromMediaStore(context: Context): List<Item> {
        return queryStore(
            context,
            MediaStore.Audio.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY),
            "$MUSIC/$ROOT/%",
            Kind.MUSIC,
        )
    }

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun queryStore(context: Context, collection: Uri, like: String, kind: Kind): List<Item> {
        val columns = arrayOf(
            MediaStore.MediaColumns._ID,
            MediaStore.MediaColumns.DISPLAY_NAME,
            MediaStore.MediaColumns.RELATIVE_PATH,
            MediaStore.MediaColumns.SIZE,
            MediaStore.MediaColumns.DATE_ADDED,
        )
        val where = "${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?"
        val args = arrayOf(like)
        val order = "${MediaStore.MediaColumns.DATE_ADDED} DESC"

        val found = mutableListOf<Item>()
        runCatching {
            context.contentResolver.query(collection, columns, where, args, order)?.use { cursor ->
                val id = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns._ID)
                val name = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.DISPLAY_NAME)
                val path = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.RELATIVE_PATH)
                val size = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.SIZE)
                val added = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.DATE_ADDED)
                while (cursor.moveToNext()) {
                    val row = cursor.getLong(id)
                    found += Item(
                        id = row,
                        uri = ContentUris.withAppendedId(collection, row),
                        name = cursor.getString(name).orEmpty(),
                        folder = folderIn(cursor.getString(path).orEmpty(), kind),
                        bytes = cursor.getLong(size),
                        addedAt = cursor.getLong(added),
                        kind = kind,
                    )
                }
            }
        }
        return found
    }

    private fun fromDisk(): List<Item> {
        val found = mutableListOf<Item>()
        fun sweep(root: File, kind: Kind) {
            if (!root.isDirectory) return
            fun walk(dir: File, folder: String) {
                dir.listFiles()?.forEach { file ->
                    when {
                        file.isDirectory -> walk(file, file.name)
                        file.isFile -> found += Item(
                            id = file.absolutePath.hashCode().toLong(),
                            uri = Uri.fromFile(file),
                            name = file.name,
                            folder = folder,
                            bytes = file.length(),
                            addedAt = file.lastModified() / 1000,
                            kind = kind,
                        )
                    }
                }
            }
            walk(root, "")
        }
        sweep(legacyRoot(), Kind.VIDEO)
        sweep(musicLegacyRoot(), Kind.MUSIC)
        return found.sortedByDescending { it.addedAt }
    }
}
