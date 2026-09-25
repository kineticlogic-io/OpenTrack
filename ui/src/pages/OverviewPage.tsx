import type { ServerStatus } from '../api/client'
import { StatusPanel } from './StatusPanel'
import { TrackLookup } from './TrackLookup'

export function OverviewPage({ onStatus }: { onStatus: (s: ServerStatus | null) => void }) {
  return (
    <div className="grid-2">
      <StatusPanel onStatus={onStatus} />
      <TrackLookup />
    </div>
  )
}
