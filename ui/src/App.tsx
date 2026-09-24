import { useCallback, useState } from 'react'
import { TbMoon, TbSun } from 'react-icons/tb'
import { Badge, Button, useTheme } from 'staresdk'
import type { ServerStatus } from './api/client'
import { StatusPanel } from './pages/StatusPanel'
import { TrackLookup } from './pages/TrackLookup'

export default function App() {
  const { theme, commitTheme } = useTheme()
  const [status, setStatus] = useState<ServerStatus | null>(null)
  const onStatus = useCallback((s: ServerStatus | null) => setStatus(s), [])

  return (
    <div className="shell">
      <header className="topbar">
        <h1>OpenTrack</h1>
        {status && (
          <Badge color="brand" outline title="GOLD site code">
            site {status.site}
          </Badge>
        )}
        <span className="spacer" />
        <Button
          size="xs"
          variant="ghost"
          icon={theme === 'dark' ? <TbSun /> : <TbMoon />}
          aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
          title={theme === 'dark' ? 'Light theme' : 'Dark theme'}
          onClick={() => commitTheme(theme === 'dark' ? 'light' : 'dark')}
        />
      </header>
      <main className="content">
        <StatusPanel onStatus={onStatus} />
        <TrackLookup />
      </main>
    </div>
  )
}
