import { lazy, Suspense, useCallback, useEffect, useState } from 'react'
import { TbMoon, TbSun } from 'react-icons/tb'
import { Badge, Button, DockProvider, PageHeader, Tabs, useTheme } from 'staresdk'
import { api, type Banner, type ServerStatus } from './api/client'
import { useHashView } from './lib/hashView'
import { OverviewPage } from './pages/OverviewPage'
import { UserChip } from './auth/UserChip'

// Code editor and map (CodeMirror, MapLibre) load only with the pages that use them.
const SourcesPage = lazy(() => import('./pages/sources/SourcesPage'))
const SchemaPage = lazy(() => import('./pages/schema/SchemaPage'))
const TrackDbPage = lazy(() => import('./pages/trackdb/TrackDbPage'))
const CorrelationPage = lazy(() => import('./pages/correlation/CorrelationPage'))
const RegistryPage = lazy(() => import('./pages/registry/RegistryPage'))
const SettingsPage = lazy(() => import('./pages/settings/SettingsPage'))

const VIEWS = [
  { id: 'overview', label: 'Overview' },
  { id: 'sources', label: 'Sources' },
  { id: 'correlation', label: 'Correlation' },
  { id: 'tracks', label: 'Track Management' },
  { id: 'registry', label: 'Registry' },
  { id: 'schema', label: 'Schema' },
  { id: 'settings', label: 'Settings' },
]

export default function App() {
  const { theme, commitTheme } = useTheme()
  const { view, sub, go } = useHashView('overview')
  const [status, setStatus] = useState<ServerStatus | null>(null)
  const onStatus = useCallback((s: ServerStatus | null) => setStatus(s), [])
  const active = VIEWS.some((v) => v.id === view) ? view : 'overview'
  // Links from before the Track Database replaced the Cards workspace.
  useEffect(() => {
    if (view === 'cards') go('tracks')
  }, [view, go])
  useEffect(() => {
    api.status().then(setStatus, () => setStatus(null))
  }, [])
  // Site name and classification banner (Settings), this instance's own.
  const [siteName, setSiteName] = useState('')
  const [banner, setBanner] = useState<Banner | null>(null)
  const [settingsRev, setSettingsRev] = useState(0)
  useEffect(() => {
    const load = () => {
      api.appSettings().then((r) => setSiteName(r.settings.site_name), () => {})
      api.banner().then((b) => setBanner(b.enabled ? b : null), () => {})
    }
    load()
    const t = setInterval(load, 60_000)
    return () => clearInterval(t)
  }, [settingsRev])
  useEffect(() => {
    document.title = siteName ? `OpenTrack · ${siteName}` : 'OpenTrack'
  }, [siteName])

  return (
    <DockProvider>
      {banner && (
        <>
          <div className="classification-bar top" style={{ background: banner.background, color: banner.color }}>
            {banner.text}
          </div>
          <div className="classification-bar bottom" style={{ background: banner.background, color: banner.color }}>
            {banner.text}
          </div>
        </>
      )}
      <div className={banner ? 'page with-banner' : 'page'}>
        <PageHeader
          title={siteName ? `OpenTrack · ${siteName}` : 'OpenTrack'}
          appName="OpenTrack"
          actions={
            <div className="num-row">
            <UserChip />
            <Button
              size="xs"
              variant="ghost"
              icon={theme === 'dark' ? <TbSun /> : <TbMoon />}
              aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
              title={theme === 'dark' ? 'Light theme' : 'Dark theme'}
              onClick={() => commitTheme(theme === 'dark' ? 'light' : 'dark')}
            />
            </div>
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
            {active === 'correlation' && <CorrelationPage />}
            {active === 'tracks' && <TrackDbPage selected={sub} onSelect={(id) => go('tracks', id)} />}
            {active === 'registry' && <RegistryPage />}
            {active === 'schema' && <SchemaPage />}
            {active === 'settings' && <SettingsPage onSaved={() => setSettingsRev((n) => n + 1)} />}
          </Suspense>
        </main>
      </div>
    </DockProvider>
  )
}
