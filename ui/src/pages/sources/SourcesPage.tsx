import { useCallback, useEffect, useState } from 'react'
import { TbPlus } from 'react-icons/tb'
import { Badge, Button, DataTable, type DataTableColumn } from 'staresdk'
import { api, type SourceRow } from '../../api/client'
import { ago, errorMessage, fmtCount } from '../../lib/format'
import { sourceState } from '../../lib/sourceState'
import { AddSourceWizard } from './AddSourceWizard'
import { SourceDetail } from './SourceDetail'

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
  { key: 'transport', header: 'Transport', width: 84, render: (s) => s.transport, sortValue: (s) => s.transport },
  {
    key: 'state',
    header: 'State',
    width: 92,
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
    width: 76,
    align: 'right',
    render: (s) => fmtCount(s.status?.totals_since_start.emitted),
    sortValue: (s) => s.status?.totals_since_start.emitted ?? null,
  },
  {
    key: 'last',
    header: 'Last',
    width: 56,
    align: 'right',
    render: (s) => ago(s.status?.link.last_frame_at),
  },
]

export default function SourcesPage({ selected, onSelect }: { selected: string; onSelect: (id: string) => void }) {
  const [sources, setSources] = useState<SourceRow[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)

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

  if (adding) {
    return (
      <AddSourceWizard
        onCancel={() => setAdding(false)}
        onDone={(id) => {
          setAdding(false)
          load()
          onSelect(id)
        }}
      />
    )
  }

  const current = sources?.find((s) => s.id === selected) ?? null
  return (
    <div className="split">
      <section className="section" aria-labelledby="sources-heading">
        <div className="section-head">
          <h2 id="sources-heading">Sources</h2>
          {sources && <span className="muted">{sources.length}</span>}
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
          selectedKey={current?.id}
          onRowClick={(s) => onSelect(s.id)}
          empty={sources ? 'No sources yet. Add one to start onboarding a feed.' : 'Loading…'}
        />
      </section>
      {current ? (
        <SourceDetail key={current.id} source={current} onChanged={load} onDeleted={() => { load(); onSelect('') }} />
      ) : (
        <section className="section">
          <span className="muted">Select a source to see its status, pipeline, preview and history.</span>
        </section>
      )}
    </div>
  )
}
