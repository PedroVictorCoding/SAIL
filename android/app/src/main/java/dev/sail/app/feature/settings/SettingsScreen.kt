package dev.sail.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailTopBar
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.sailViewModel

@Composable
fun SettingsScreen(onDone: () -> Unit, onBack: (() -> Unit)?) {
    val vm = sailViewModel { SettingsViewModel(it.settings, it.connection) }
    val state by vm.state.collectAsStateWithLifecycle()
    Column(Modifier.fillMaxSize()) {
        SailTopBar(
            title = if (state.configured) "Settings" else "Connect to SAIL",
            subtitle = "Server and owner token",
            onBack = onBack,
        )
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(ScreenGutter),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text(
                "This app is a client of your own SAIL server. It shows what SAIL's APIs return " +
                    "and keeps nothing else on the phone.",
                color = SailColors.TextMuted,
                fontSize = 14.sp,
            )
            OutlinedTextField(
                value = state.url,
                onValueChange = vm::onUrlChange,
                label = { Text("Server address") },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = state.token,
                onValueChange = vm::onTokenChange,
                label = { Text("Owner token") },
                singleLine = true,
                enabled = !state.busy,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                modifier = Modifier.fillMaxWidth(),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                SecondaryButton("Get token from SAIL", onClick = vm::fetchToken, enabled = !state.busy)
                PrimaryButton(
                    if (state.busy) "Checking…" else "Connect",
                    onClick = { vm.save(onDone) },
                    enabled = !state.busy,
                )
            }
            state.message?.let { message ->
                Text(
                    message,
                    color = if (state.messageIsError) SailColors.Error else SailColors.Good,
                    fontSize = 14.sp,
                )
            }
            if (!state.persistent) {
                FlatCard {
                    Text(
                        "This phone's encrypted storage isn't available, so the token is kept " +
                            "in memory only and you'll need to enter it again next time.",
                        color = SailColors.Warning,
                        fontSize = 13.sp,
                    )
                }
            }
            if (state.configured) {
                SecondaryButton("Forget this server", onClick = vm::forget, enabled = !state.busy)
            }
        }
    }
}
