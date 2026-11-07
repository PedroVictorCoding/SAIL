package dev.sail.app.feature.dialogue

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import java.io.ByteArrayOutputStream

/** Images for SAIL's vision: JPEG, longest side ≤ 1600 px, under the server's 4 MB limit. */
object Jpeg {
    private const val MAX_SIDE = 1600
    private const val MAX_BYTES = 4_000_000

    fun fromBitmap(bitmap: Bitmap): ByteArray? {
        val scaled = scale(bitmap)
        for (quality in intArrayOf(85, 70, 55)) {
            val out = ByteArrayOutputStream()
            scaled.compress(Bitmap.CompressFormat.JPEG, quality, out)
            if (out.size() <= MAX_BYTES) return out.toByteArray()
        }
        return null
    }

    fun fromUri(context: Context, uri: Uri): ByteArray? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        context.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= MAX_SIDE) sample *= 2
        val options = BitmapFactory.Options().apply { inSampleSize = sample }
        val bitmap = context.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, options) }
            ?: return null
        return fromBitmap(bitmap)
    }

    fun decode(bytes: ByteArray): Bitmap? = BitmapFactory.decodeByteArray(bytes, 0, bytes.size)

    private fun scale(bitmap: Bitmap): Bitmap {
        val side = maxOf(bitmap.width, bitmap.height)
        if (side <= MAX_SIDE) return bitmap
        val ratio = MAX_SIDE.toFloat() / side
        return Bitmap.createScaledBitmap(bitmap, (bitmap.width * ratio).toInt(), (bitmap.height * ratio).toInt(), true)
    }
}
