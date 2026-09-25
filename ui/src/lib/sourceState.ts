import type { BadgeColor } from 'staresdk'
import type { SourceRow } from '../api/client'

/** Running / starting / stopped / failing, from config plus the worker's live status. */
export function sourceState(s: SourceRow): { label: string; color: BadgeColor } {
  if (!s.enabled) return { label: 'disabled', color: 'grey' }
  if (!s.status) return { label: 'starting', color: 'warning' }
  if (s.status.link.connected) return { label: 'running', color: 'success' }
  return { label: s.status.link.errors > 0 ? 'failing' : 'connecting', color: s.status.link.errors > 0 ? 'danger' : 'warning' }
}
