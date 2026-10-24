package dev.forgesworn.sapflow.broadcaster

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.unit.dp
import java.text.DateFormat
import java.util.Date
import java.util.Locale

private sealed interface LibraryRow {
    data class Show(val key: String, val title: String) : LibraryRow
    data class Segment(val entry: RecordingCatalogue.Entry) : LibraryRow
}

/** A private local list. The Activity owns the stable document-picker route;
 * rotating this panel cannot replace the exact pending export selection. */
@Composable
internal fun RecordingLibraryPanel(
    visible: Boolean,
    entries: List<RecordingCatalogue.Entry>,
    busy: Boolean,
    message: String?,
    canExport: Boolean,
    onDismiss: () -> Unit,
    onRefresh: () -> Unit,
    onExport: (RecordingCatalogue.Entry, Boolean) -> Unit,
    onCancelExport: () -> Unit,
) {
    var unverified by remember { mutableStateOf<RecordingCatalogue.Entry?>(null) }
    if (!visible) return
    val listHeight = (LocalConfiguration.current.screenHeightDp.dp - 280.dp).coerceIn(90.dp, 300.dp)
    val rows = remember(entries) {
        val date = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT)
        entries.groupBy { it.identity?.showId ?: it.segmentId }.flatMap { (key, parts) ->
            val first = parts.mapNotNull { it.createdAtMs }.minOrNull()
            val title = first?.let { date.format(Date(it)) } ?: "Unverified recording"
            listOf<LibraryRow>(LibraryRow.Show(key, title)) + parts.sortedBy { it.identity?.segmentIndex }.map(LibraryRow::Segment)
        }
    }
    AlertDialog(
        onDismissRequest = { unverified = null; onDismiss() },
        title = { Text("Private recordings") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Recordings stay on this device until you choose Export. Export keeps the private original.")
                if (!canExport) Text("Stop and save capture before opening the document picker.")
                if (message != null) Text(message)
                if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (entries.isEmpty() && !busy) Text("No recording files found.")
                LazyColumn(Modifier.fillMaxWidth().height(listHeight), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(rows, key = { row -> when (row) {
                        is LibraryRow.Show -> "show:${row.key}"
                        is LibraryRow.Segment -> "part:${row.entry.segmentId}"
                    } }) { row -> when (row) {
                        is LibraryRow.Show -> Text(row.title, style = MaterialTheme.typography.titleSmall)
                        is LibraryRow.Segment -> {
                            val entry = row.entry
                            val info = remember(entry) {
                                val part = entry.identity?.segmentIndex?.let { "Part ${it + 1} · " } ?: ""
                                val size = String.format(Locale.getDefault(), "%.1f MiB", entry.bytes / 1_048_576.0)
                                val duration = entry.durationMs?.let { " · ${it / 1000}s" } ?: ""
                                part + size + duration
                            }
                            Column {
                                Text(info, style = MaterialTheme.typography.bodySmall)
                                Text(when (entry.state) {
                                    RecordingCatalogue.State.WRITING -> "Recording in progress"
                                    RecordingCatalogue.State.FINALISED -> "Finalised by the recorder"
                                    RecordingCatalogue.State.INCOMPLETE -> "Unfinished; playback unverified"
                                    RecordingCatalogue.State.ORPHAN_UNVERIFIED -> "Completion unknown; playback unverified"
                                }, style = MaterialTheme.typography.bodySmall)
                                OutlinedButton(enabled = canExport && !busy && entry.bytes > 0 && entry.state != RecordingCatalogue.State.WRITING,
                                    onClick = { if (entry.state == RecordingCatalogue.State.FINALISED) onExport(entry, false) else unverified = entry }) {
                                    Text(if (entry.state == RecordingCatalogue.State.FINALISED) "Export recording" else "Export unverified file")
                                }
                            }
                        }
                    } }
                }
            }
        },
        confirmButton = { TextButton(onClick = { unverified = null; onDismiss() }) { Text("Close") } },
        dismissButton = {
            if (busy) TextButton(onClick = onCancelExport) { Text("Cancel file operation") }
            else TextButton(onClick = onRefresh) { Text("Refresh") }
        },
    )
    unverified?.let { exact ->
        AlertDialog(onDismissRequest = { unverified = null }, title = { Text("Export an unverified file?") },
            text = { Text("This file was not confirmed complete. It may contain only part of the recording and may not play. Export copies it without repairing or changing the original.") },
            confirmButton = { TextButton(enabled = !busy && canExport, onClick = { unverified = null; onExport(exact, true) }) { Text("Export unverified file") } },
            dismissButton = { TextButton(onClick = { unverified = null }) { Text("Cancel") } })
    }
}
