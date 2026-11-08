package dev.sail.app.core.net

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ServerSettingsTest {
    @Test
    fun parseUrlAddsHttpsAndDropsTrailingSlash() {
        assertEquals("https://sail.example.com/", ServerSettings.parseUrl("sail.example.com/")?.toString())
        assertEquals("http://100.64.0.2:8443/", ServerSettings.parseUrl(" http://100.64.0.2:8443 ")?.toString())
    }

    @Test
    fun parseUrlRejectsNonsense() {
        assertNull(ServerSettings.parseUrl(""))
        assertNull(ServerSettings.parseUrl("ftp://sail.example.com"))
        assertNull(ServerSettings.parseUrl("https://"))
    }
}
