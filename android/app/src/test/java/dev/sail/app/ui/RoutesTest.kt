package dev.sail.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class RoutesTest {
    @Test
    fun subRoutesSelectTheirTab() {
        assertEquals(TopLevel.Arena, TopLevel.of(Routes.LESSON))
        assertEquals(TopLevel.Arena, TopLevel.of(Routes.COURSE))
        assertEquals(TopLevel.Library, TopLevel.of(Routes.BOOK))
        assertEquals(TopLevel.Library, TopLevel.of(Routes.READ))
        assertEquals(TopLevel.Home, TopLevel.of(Routes.TRACKER))
        assertNull(TopLevel.of(Routes.SETTINGS))
    }
}
