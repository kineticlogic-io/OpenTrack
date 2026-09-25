import { lazy, Suspense, useCallback, useEffect, useState } from 'react'
import { TbLayoutDashboard, TbMoon, TbPlugConnected, TbSchema, TbSun } from 'react-icons/tb'
import { Badge, Button, Tabs, useTheme } from 'staresdk'
import { api, type ServerStatus } from './api/client'
import { useHashView } from './lib/hashView'
import { OverviewPage } from './pages/OverviewPage'

// Code editor and map (CodeMirror, MapLibre) load only with the pages that use them.
const SourcesPage = lazy(() => import('./pages/sources/SourcesPage'))
const SchemaPage = lazy(() => import('./pages/schema/SchemaPage'))

const VIEWS = [
  { id: 'overview', label: 'Overview', icon: <TbLayoutDashboard /> },
  { id: 'sources', label: 'Sources', icon: <TbPlugConnected /> },
  { id: 'schema', label: 'Schema', icon: <TbSchema /> },
]

export default function App() {
  const { theme, commitTheme } = useTheme()
  const { view, sub, go } = useHashView('overview')
  const [status, setStatus] = useState<ServerStatus | null>(null)
  const onStatus = useCallback((s: ServerStatus | null) => setStatus(s), [])
  const active = VIEWS.some((v) => v.id === view) ? view : 'overview'
  useEffect(() => {
    api.status().then(setStatus, () => setStatus(null))
  }, [])

  return (
    <div className="shell">
      <header className="topbar">
        <h1>OpenTrack</h1>
        {status && (
          <Badge color="grey" outline size="sm" title="GOLD site code">
            site {status.site}
          </Badge>
        )}
        <Tabs
          aria-label="Workspaces"
          size="sm"
          value={active}
          onChange={(id) => go(id)}
          tabs={VIEWS}
          style={{ borderBottom: 'none', height: '100%', marginLeft: 'var(--space-md)' }}
        />
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
      <main className="workspace">
        {active === 'overview' && <OverviewPage onStatus={onStatus} />}
        <Suspense fallback={<span className="muted">Loading…</span>}>
          {active === 'sources' && <SourcesPage selected={sub} onSelect={(id) => go('sources', id)} />}
          {active === 'schema' && <SchemaPage />}
        </Suspense>
      </main>
    </div>
  )
}
