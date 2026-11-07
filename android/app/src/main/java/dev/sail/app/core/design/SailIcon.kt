package dev.sail.app.core.design

import androidx.annotation.DrawableRes
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.sail.app.R

/**
 * SAIL's icon set: the web sprite (assets/source/icons/sail-icons.svg) converted by
 * android/tools/convert_icons.py, plus the nabla mark. Add an icon to the sprite, re-run the
 * script, then add it here.
 */
enum class SailIcon(@DrawableRes val res: Int) {
    Home(R.drawable.ic_sail_home),
    Spaces(R.drawable.ic_sail_spaces),
    Settings(R.drawable.ic_sail_settings),
    Dialogue(R.drawable.ic_sail_dialogue),
    Canvas(R.drawable.ic_sail_canvas),
    Vision(R.drawable.ic_sail_vision),
    Arena(R.drawable.ic_sail_arena),
    Flow(R.drawable.ic_sail_flow),
    Life(R.drawable.ic_sail_life),
    Entertainment(R.drawable.ic_sail_entertainment),
    Mind(R.drawable.ic_sail_mind),
    Agents(R.drawable.ic_sail_agents),
    Guardian(R.drawable.ic_sail_guardian),
    Bridge(R.drawable.ic_sail_bridge),
    Discovery(R.drawable.ic_sail_discovery),
    Pulse(R.drawable.ic_sail_pulse),
    Sense(R.drawable.ic_sail_sense),
    Mesh(R.drawable.ic_sail_mesh),
    Lab(R.drawable.ic_sail_lab),
    Terminal(R.drawable.ic_sail_terminal),
    Core(R.drawable.ic_sail_core),
    Factory(R.drawable.ic_sail_factory),
    Forge(R.drawable.ic_sail_forge),
    Mark(R.drawable.ic_sail_mark),

    /** The web composer's microphone glyph (index.html #micBtn). */
    Mic(R.drawable.ic_mic),
}

/** [contentDescription] is required when the icon is the only label of a control. */
@Composable
fun SailIconImage(
    icon: SailIcon,
    contentDescription: String?,
    modifier: Modifier = Modifier,
    tint: Color = SailColors.Text,
    size: Dp = 22.dp,
) {
    Icon(
        painter = painterResource(icon.res),
        contentDescription = contentDescription,
        tint = tint,
        modifier = modifier.size(size),
    )
}
