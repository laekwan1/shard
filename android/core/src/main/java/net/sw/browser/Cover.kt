package net.sw.browser

/**
 * A picture put inside a song, where every player looks for one.
 *
 * `moov/udta/meta/ilst/covr/data` — long-winded, but it is the one place the
 * phone's music app, the file browser and this program all agree on. A file that
 * carries its own cover is one file; a picture kept beside it is two, and the
 * second one is lost the moment the first is copied anywhere.
 *
 * The same shape the desktop build writes, so a song saved on either reads on
 * both.
 */
object Cover {

    /** The same file with a picture in its header, or null if it cannot take one. */
    fun into(file: ByteArray, picture: ByteArray, png: Boolean): ByteArray? {
        // Only a file made of fragments, which is what a stream downloaded in
        // pieces is: in a plain MP4 the sample positions are counted from the
        // start of the file, and a longer header would move every one of them.
        if (find(file, 0, file.size, "moof") == null) return null
        if (read(file) != null) return null

        val moov = find(file, 0, file.size, "moov") ?: return null
        val start = moov.first - 8
        if (start < 0 || name(file, start + 4) != "moov") return null

        // Version zero; the flags say what the picture is: 13 a JPEG, 14 a PNG.
        val data = ByteArray(8).also { it[3] = if (png) 14 else 13 } + picture
        val covr = boxed("covr", boxed("data", data))

        // Into whatever is already there, at every level: a header holds one
        // `udta`, and a second beside the file's own is a header no reader is
        // obliged to make sense of.
        val body = grown(file, moov.first, moov.second, covr)
        return file.copyOfRange(0, start) + boxed("moov", body) + file.copyOfRange(moov.second, file.size)
    }

    /** The picture inside a file, if it has one. */
    fun read(file: ByteArray): ByteArray? {
        val moov = find(file, 0, file.size, "moov") ?: return null
        // Every one at each step, not the first: a file can arrive with a `udta`
        // of its own holding nothing this looks for.
        for (udta in all(file, moov.first, moov.second, "udta")) {
            for (meta in all(file, udta.first, udta.second, "meta")) {
                for (ilst in all(file, meta.first + 4, meta.second, "ilst")) {
                    for (covr in all(file, ilst.first, ilst.second, "covr")) {
                        val data = find(file, covr.first, covr.second, "data") ?: continue
                        if (data.first + 8 > data.second) continue
                        return file.copyOfRange(data.first + 8, data.second)
                    }
                }
            }
        }
        return null
    }

    // ---- the header, rebuilt from the inside out ---------------------------

    private fun grown(file: ByteArray, body: Int, end: Int, covr: ByteArray): ByteArray {
        val udta = find(file, body, end, "udta")
            ?: return file.copyOfRange(body, end) + boxed("udta", freshMeta(covr))
        val udtaStart = udta.first - 8

        val meta = find(file, udta.first, udta.second, "meta")
        val inner = if (meta == null) {
            file.copyOfRange(udta.first, udta.second) + freshMeta(covr)
        } else {
            val metaStart = meta.first - 8
            val ilst = find(file, meta.first + 4, meta.second, "ilst")
            val held = if (ilst == null) {
                file.copyOfRange(meta.first, meta.second) + boxed("ilst", covr)
            } else {
                val grownIlst = boxed("ilst", file.copyOfRange(ilst.first, ilst.second) + covr)
                swapped(file, meta.first, meta.second, ilst.first - 8, ilst.second, grownIlst)
            }
            swapped(file, udta.first, udta.second, metaStart, meta.second, boxed("meta", held))
        }
        return swapped(file, body, end, udtaStart, udta.second, boxed("udta", inner))
    }

    private fun freshMeta(covr: ByteArray): ByteArray {
        val hdlr = ByteArray(8) + "mdirappl".toByteArray(Charsets.US_ASCII) + ByteArray(13)
        return boxed("meta", ByteArray(4) + boxed("hdlr", hdlr) + boxed("ilst", covr))
    }

    /** A container's body with one of its children put back a different size. */
    private fun swapped(
        file: ByteArray,
        body: Int,
        end: Int,
        child: Int,
        childEnd: Int,
        with: ByteArray,
    ): ByteArray = file.copyOfRange(body, child) + with + file.copyOfRange(childEnd, end)

    private fun boxed(kind: String, body: ByteArray): ByteArray {
        val size = body.size + 8
        return byteArrayOf(
            (size ushr 24).toByte(), (size ushr 16).toByte(), (size ushr 8).toByte(), size.toByte(),
        ) + kind.toByteArray(Charsets.US_ASCII) + body
    }

    // ---- walking the boxes -------------------------------------------------

    private fun name(file: ByteArray, at: Int) =
        String(file, at, 4, Charsets.US_ASCII)

    /** Body and end of the first named box among the children of a range. */
    private fun find(file: ByteArray, from: Int, to: Int, want: String): Pair<Int, Int>? =
        all(file, from, to, want).firstOrNull()

    private fun all(file: ByteArray, from: Int, to: Int, want: String): List<Pair<Int, Int>> {
        val found = mutableListOf<Pair<Int, Int>>()
        var at = from
        while (at + 8 <= to) {
            var size = be32(file, at)
            var body = at + 8
            if (size == 1L) {
                if (at + 16 > to) return found
                size = be64(file, at + 8)
                body = at + 16
            } else if (size == 0L) {
                size = (to - at).toLong()
            }
            val stop = at + size.toInt()
            if (size < 8 || stop > to || stop <= at) return found
            if (name(file, at + 4) == want) found.add(body to stop)
            at = stop
        }
        return found
    }

    private fun be32(file: ByteArray, at: Int): Long {
        var v = 0L
        for (i in 0 until 4) v = (v shl 8) or (file[at + i].toLong() and 0xff)
        return v
    }

    private fun be64(file: ByteArray, at: Int): Long {
        var v = 0L
        for (i in 0 until 8) v = (v shl 8) or (file[at + i].toLong() and 0xff)
        return v
    }
}
