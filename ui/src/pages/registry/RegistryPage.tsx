import { useCallback, useEffect, useRef, useState } from 'react'
import { TbDownload, TbSearch, TbUpload } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, Input, useToast, type DataTableColumn } from 'staresdk'
import { api, type RegistryListEntity, type SheetImport } from '../../api/client'
import { errorMessage, fmtTime } from '../../lib/format'
import { CardEditor } from '../trackdb/CardEditor'

const PAGE = 200

const COLUMNS: DataTableColumn<RegistryListEntity>[] = [
  { key: 'name', header: 'Name', render: (e) => e.name ?? <span className="muted">{e.id}</span>, sortValue: (e) => e.name ?? e.id },
  {
    key: 'ids',
    header: 'Identifiers',
    mono: true,
    render: (e) => e.identifiers.map((i) => `${i.scheme}:${i.value}`).join('  '),
  },
  {
    key: 'fields',
    header: 'Registry',
    render: (e) =>
      Object.entries(e.fields)
        .map(([k, v]) => `${k} ${typeof v === 'string' ? v : JSON.stringify(v)}`)
        .join(' · '),
  },
  {
    key: 'card',
    header: 'Card',
    width: 110,
    render: (e) => {
      const n = Object.keys(e.card ?? {}).length
      return n ? <span title={e.card_updated_at_ms ? `saved ${fmtTime(e.card_updated_at_ms)}` : undefined}>{n} field{n === 1 ? '' : 's'}</span> : <span className="muted">—</span>
    },
    sortValue: (e) => Object.keys(e.card ?? {}).length,
  },
  {
    key: 'status',
    header: 'Status',
    width: 80,
    render: (e) => <Badge size="sm" color={e.status === 'active' ? 'grey' : 'warning'}>{e.status}</Badge>,
  },
]

const ACTION_COLOR = { create: 'success', update: 'blue', unchanged: 'grey', error: 'danger' } as const

/** The registry: every entity with its identifiers, registry fields and card; spreadsheet export and import. */
export default function RegistryPage() {
  const { toast } = useToast()
  const [query, setQuery] = useState('')
  const [rows, setRows] = useState<RegistryListEntity[] | null>(null)
  const [total, setTotal] = useState(0)
  const [editing, setEditing] = useState<RegistryListEntity | null>(null)
  const [plan, setPlan] = useState<{ file: File; result: SheetImport } | null>(null)
  const [busy, setBusy] = useState(false)
  const picker = useRef<HTMLInputElement>(null)

  const load = useCallback(() => {
    api.registryEntities(query.trim(), PAGE).then(
      (r) => {
        setRows(r.entities)
        setTotal(r.total)
      },
      (e) => toast({ variant: 'error', title: 'Registry', message: errorMessage(e) }),
    )
  }, [query, toast])
  useEffect(() => {
    const t = setTimeout(load, 250)
    return () => clearTimeout(t)
  }, [load])

  const preview = async (file: File) => {
    setBusy(true)
    try {
      setPlan({ file, result: await api.importSheet(file, false) })
    } catch (e) {
      toast({ variant: 'error', title: 'Not a registry sheet', message: errorMessage(e) })
    } finally {
      setBusy(false)
    }
  }
  const apply = async () => {
    if (!plan) return
    setBusy(true)
    try {
      const r = await api.importSheet(plan.file, true)
      if (!r.applied) {
        setPlan({ file: plan.file, result: r })
        toast({ variant: 'error', title: 'Not imported', message: 'Some rows have errors; nothing was written.' })
        return
      }
      toast({ variant: 'success', title: 'Imported', message: `${r.counts.create} created, ${r.counts.update} updated` })
      setPlan(null)
      load()
    } catch (e) {
      toast({ variant: 'error', title: 'Not imported', message: errorMessage(e) })
    } finally {
      setBusy(false)
    }
  }

  const planRows = plan?.result.rows.filter((r) => r.action !== 'unchanged') ?? []
  return (
    <div className="stack">
      <CollapsiblePanel
        title="Registry"
        badge={rows ? (query.trim() ? `${rows.length.toLocaleString()} of ${total.toLocaleString()}` : total.toLocaleString()) : undefined}
        persistKey="ot.panel.registry"
        titleActions={
          <div className="search">
            <TbSearch aria-hidden />
            <Input
              aria-label="Search the registry"
              placeholder="Name or identifier"
              style={{ paddingLeft: 26 }}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
          </div>
        }
        actions={
          <>
            <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.registryExportUrl('xlsx'), '_self')}>
              Export XLSX
            </Button>
            <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.registryExportUrl('csv'), '_self')}>
              Export CSV
            </Button>
            <Button size="sm" icon={<TbUpload />} disabled={busy} onClick={() => picker.current?.click()}>
              Import sheet
            </Button>
            <input
              ref={picker}
              type="file"
              accept=".xlsx,.csv"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0]
                e.target.value = ''
                if (f) preview(f)
              }}
            />
          </>
        }
      >
        <span className="muted">
          One row per entity: <span className="mono">entity_id</span>, <span className="mono">name</span>, <span className="mono">status</span>,{' '}
          <span className="mono">id:&lt;scheme&gt;</span> (several separated by ;), <span className="mono">registry:&lt;key&gt;</span> and{' '}
          <span className="mono">card:&lt;field&gt;</span>. Export a sheet to start from. Blank cells leave values as they are; an identifier is never
          taken from another entity.
        </span>
        <DataTable
          aria-label="Registry entities"
          columns={COLUMNS}
          rows={rows ?? []}
          rowKey={(e) => e.id}
          onRowClick={(e) => setEditing(e)}
          empty={rows ? 'No entity matches.' : 'Loading…'}
          maxHeight={560}
        />
        {rows && total > rows.length && <span className="muted">Showing the first {rows.length.toLocaleString()}; search to narrow.</span>}
      </CollapsiblePanel>

      {plan && (
        <CollapsiblePanel
          title={`Import ${plan.file.name}`}
          badge={`${plan.result.counts.create} new · ${plan.result.counts.update} updated · ${plan.result.counts.unchanged} unchanged · ${plan.result.counts.error} errors`}
          persistKey="ot.panel.registryimport"
          actions={
            <>
              <Button size="sm" variant="ghost" onClick={() => setPlan(null)}>
                Cancel
              </Button>
              <Button size="sm" disabled={busy || plan.result.counts.error > 0 || planRows.length === 0} onClick={apply}>
                Apply
              </Button>
            </>
          }
        >
          {plan.result.counts.error > 0 && <div className="error-text">Fix the rows with errors and import again: nothing is written while any row has one.</div>}
          <DataTable
            aria-label="Import plan"
            columns={[
              { key: 'row', header: 'Row', width: 56, align: 'right', render: (r) => r.row },
              { key: 'action', header: 'Change', width: 96, render: (r) => <Badge size="sm" color={ACTION_COLOR[r.action]}>{r.action}</Badge> },
              { key: 'entity', header: 'Entity', render: (r) => r.name ?? r.entity_id },
              {
                key: 'what',
                header: 'What',
                render: (r) =>
                  r.errors?.length ? (
                    <span className="error-text">{r.errors.join('; ')}</span>
                  ) : (
                    [
                      r.identifiers_added?.length ? `+ ${r.identifiers_added.join(', ')}` : '',
                      r.registry_fields?.length ? `registry ${r.registry_fields.join(', ')}` : '',
                      r.card_fields?.length ? `card ${r.card_fields.join(', ')}` : '',
                    ]
                      .filter(Boolean)
                      .join(' · ')
                  ),
              },
            ]}
            rows={planRows}
            rowKey={(r) => String(r.row)}
            empty="Nothing to change."
            maxHeight={360}
          />
        </CollapsiblePanel>
      )}

      <CardEditor
        entityId={editing?.id ?? null}
        fromTrack=""
        title={editing?.name ?? editing?.id ?? ''}
        open={editing !== null}
        onClose={() => setEditing(null)}
        onSaved={() => load()}
      />
    </div>
  )
}
