import { lazy, Suspense, useCallback, useEffect, useState } from 'react'
import { TbPlus } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, type DataTableColumn } from 'staresdk'
import { api, type SourceRow } from '../../api/client'
import { ago, errorMessage, fmtCount } from '../../lib/format'
import { sourceState } from '../../lib/sourceState'
import { AddSourceWizard } from './AddSourceWizard'
import { SourceDetail } from './SourceDetail'

// React Flow loads only with the Sources page's topology.
const Topology = lazy(() => import('./Topology'))

const REFRESH_MS = 5000

const COLUMNS: DataTableColumn<SourceRow>[] = [
  {
    key: 'name',
    header: 'Source',
    render: (s) => (
      <span>
        {s.name} <span className="muted mono">{s.id}</span>
      </span>
    ),
    sortValue: (s) => s.name,
  },
  { key: 'transport', header: 'Transport', width: 110, render: (s) => s.transport, sortValue: (s) => s.transport },
  {
    key: 'state',
    header: 'State',
    width: 100,
    render: (s) => {
      const st = sourceState(s)
      return (
        <Badge color={st.color} size="sm" uppercase>
          {st.label}
        </Badge>
      )
    },
    sortValue: (s) => sourceState(s).label,
  },
  {
    key: 'emitted',
    header: 'Emitted',
    width: 100,
    align: 'right',
    render: (s) => fmtCount(s.status?.totals_since_start.emitted),
    sortValue: (s) => s.status?.totals_since_start.emitted ?? null,
  },
  {
    key: 'last',
    header: 'Last frame',
    width: 100,
    align: 'right',
    render: (s) => ago(s.status?.link.last_frame_at),
  },
]

export default function SourcesPage({ selected, onSelect }: { selected: string; onSelect: (id: string) => void }) {
  const [sources, setSources] = useState<SourceRow[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)
  // The drawer keeps showing the last source while it slides closed.
  const [shownId, setShownId] = useState(selected)

  const load = useCallback(() => {
    api.sources().then(
      (s) => {
        setSources(s)
        setError(null)
      },
      (e) => setError(errorMessage(e)),
    )
  }, [])

  useEffect(() => {
    load()
    const t = setInterval(load, REFRESH_MS)
    return () => clearInterval(t)
  }, [load])

  const select = (id: string) => {
    if (id) setShownId(id)
    onSelect(id)
  }

  if (adding) {
    return (
      <div className="panels">
        <AddSourceWizard
          onCancel={() => setAdding(false)}
          onDone={(id) => {
            setAdding(false)
            load()
            select(id)
          }}
        />
      </div>
    )
  }

  const shown = sources?.find((s) => s.id === (selected || shownId)) ?? null
  const open = !!selected && !!shown
  return (
    <div className="panels">
      <CollapsiblePanel title="Topology" persistKey="ot.panel.topology">
        <div className="panel-body">
          <Suspense fallback={<span className="muted">LOADING…</span>}>
            {sources ? <Topology sources={sources} onSelect={select} /> : <span className="muted">LOADING…</span>}
          </Suspense>
        </div>
      </CollapsiblePanel>
      <CollapsiblePanel title="Sources" badge={sources ? String(sources.length) : undefined} persistKey="ot.panel.sources">
        <div className="panel-body">
          <div className="toolbar">
            <span className="muted">Feeds OpenTrack ingests. Select one to see its status, transport, pipeline and history.</span>
            <span className="spacer" />
            <Button size="sm" icon={<TbPlus />} onClick={() => setAdding(true)}>
              Add source
            </Button>
          </div>
          {error && <div className="error-text">{error}</div>}
          <DataTable
            aria-label="Sources"
            columns={COLUMNS}
            rows={sources ?? []}
            rowKey={(s) => s.id}
            selectedKey={open ? shown?.id : null}
            onRowClick={(s) => select(s.id)}
            empty={sources ? 'No sources yet. Add one to start onboarding a feed.' : 'LOADING…'}
          />
        </div>
      </CollapsiblePanel>
      {shown && (
        <SourceDetail
          key={shown.id}
          source={shown}
          open={open}
          onClose={() => select('')}
          onChanged={load}
          onDeleted={() => {
            load()
            onSelect('')
            setShownId('')
          }}
        />
      )}
    </div>
  )
}
