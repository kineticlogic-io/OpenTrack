import { lazy, Suspense, useCallback, useEffect, useState } from 'react'
import { TbMoon, TbSun } from 'react-icons/tb'
import { Badge, Button, DockProvider, PageHeader, Tabs, useTheme } from 'staresdk'
import { api, type ServerStatus } from './api/client'
import { useHashView } from './lib/hashView'
import { OverviewPage } from './pages/OverviewPage'

// Code editor and map (CodeMirror, MapLibre) load only with the pages that use them.
const SourcesPage = lazy(() => import('./pages/sources/SourcesPage'))
const SchemaPage = lazy(() => import('./pages/schema/SchemaPage'))
const CardsPage = lazy(() => import('./pages/cards/CardsPage'))

const VIEWS = [
  { id: 'overview', label: 'Overview' },
  { id: 'sources', label: 'Sources' },
  { id: 'cards', label: 'Cards' },
  { id: 'schema', label: 'Schema' },
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
    <DockProvider>
      <div className="page">
        <PageHeader
          title="OpenTrack"
          appName="OpenTrack"
          actions={
            <Button
              size="xs"
              variant="ghost"
              icon={theme === 'dark' ? <TbSun /> : <TbMoon />}
              aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
              title={theme === 'dark' ? 'Light theme' : 'Dark theme'}
              onClick={() => commitTheme(theme === 'dark' ? 'light' : 'dark')}
            />
          }
        >
          {status && (
            <Badge color="grey" size="sm" title="GOLD site code">
              site {status.site}
            </Badge>
          )}
        </PageHeader>
        <Tabs variant="bar" aria-label="Workspaces" value={active} onChange={(id) => go(id)} tabs={VIEWS} />
        <main className="content">
          {active === 'overview' && <OverviewPage onStatus={onStatus} />}
          <Suspense fallback={<span className="muted">LOADING…</span>}>
            {active === 'sources' && <SourcesPage selected={sub} onSelect={(id) => go('sources', id)} />}
            {active === 'cards' && <CardsPage selected={sub} onSelect={(id) => go('cards', id)} />}
            {active === 'schema' && <SchemaPage />}
          </Suspense>
        </main>
      </div>
    </DockProvider>
  )
}
