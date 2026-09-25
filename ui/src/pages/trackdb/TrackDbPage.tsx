import { useEffect, useMemo, useState } from 'react'
import { TbPencil, TbSearch } from 'react-icons/tb'
import { setWorkerUrl } from 'maplibre-gl'
import maplibreWorkerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url'
import { Badge, Button, CollapsiblePanel, DataTable, FieldSelect, Input, useToast, type DataTableColumn } from 'staresdk'
import { MapView, type MapPoint } from 'staresdk/map-view'
import { api, type TrackRow } from '../../api/client'
import { ago, errorMessage, fmtNum, STATE_COLOR } from '../../lib/format'
import { affiliationColor } from '../../lib/palette'
import { symbolUrl } from '../../lib/symbol'
import { CardEditor } from './CardEditor'
import { TrackCard } from './TrackCard'

// MapLibre resolves its worker relative to its own module, which a bundle breaks.
setWorkerUrl(maplibreWorkerUrl)

const OUTLINES = '/world-110m.geo.json'
const REFRESH_MS = 5000

const displayName = (t: TrackRow) => t.name ?? t.callsign ?? (t.gold_name !== 'UNKNOWN' ? t.gold_name : '')

const COLUMNS: DataTableColumn<TrackRow>[] = [
  {
    key: 'symbol',
    header: '',
    width: 34,
    render: (t) => {
      const url = symbolUrl(t.sidc, 18)
      return url ? <img className="symbol-cell" src={url} alt="" /> : null
    },
  },
  { key: 'track', header: 'Track', mono: true, width: 140, render: (t) => t.track_id, sortValue: (t) => t.track_id },
  { key: 'name', header: 'Name', render: displayName, sortValue: (t) => displayName(t) || null },
  { key: 'domain', header: 'Domain', width: 90, render: (t) => t.domain, sortValue: (t) => t.domain },
  { key: 'affiliation', header: 'Affiliation', width: 100, render: (t) => t.affiliation, sortValue: (t) => t.affiliation },
  {
    key: 'force',
    header: 'Force',
    width: 60,
    align: 'right',
    mono: true,
    render: (t) => String(t.force_code).padStart(2, '0'),
    sortValue: (t) => t.force_code,
  },
  {
    key: 'state',
    header: 'State',
    width: 100,
    render: (t) => (
      <Badge color={STATE_COLOR[t.state] ?? 'grey'} size="sm" uppercase>
        {t.state}
      </Badge>
    ),
    sortValue: (t) => t.state,
  },
  {
    key: 'card',
    header: 'Card',
    width: 90,
    render: (t) =>
      t.notices > 0 ? (
        <Badge color="warning" size="sm" title="Card values differ from what a feed reports">
          differs {t.notices}
        </Badge>
      ) : t.entity_id ? (
        <Badge color="blue" size="sm">
          card
        </Badge>
      ) : (
        ''
      ),
    sortValue: (t) => (t.notices > 0 ? 2 : t.entity_id ? 1 : 0),
  },
  { key: 'sources', header: 'Sources', mono: true, render: (t) => t.sources.join(', '), sortValue: (t) => t.sources[0] },
  {
    key: 'speed',
    header: 'Speed',
    width: 80,
    align: 'right',
    mono: true,
    render: (t) => fmtNum(t.speed_mps, 1, ' m/s'),
    sortValue: (t) => t.speed_mps,
  },
  {
    key: 'last',
    header: 'Last seen',
    width: 80,
    align: 'right',
    render: (t) => ago(t.last_seen),
    sortValue: (t) => new Date(t.last_seen).getTime(),
  },
]

/** A filter select whose first option means "no filter". */
function Filter({ label, any, values, value, onChange }: { label: string; any: string; values: string[]; value: string | null; onChange: (v: string | null) => void }) {
  return (
    <FieldSelect
      ariaLabel={label}
      fields={[{ name: any }, ...values.map((name) => ({ name }))]}
      value={value ?? any}
      onChange={(v) => onChange(v === any || v === null ? null : v)}
      style={{ width: 128 }}
    />
  )
}

const distinct = (rows: TrackRow[], pick: (t: TrackRow) => string[]) => [...new Set(rows.flatMap(pick))].sort()

/** Every live track on a map and in a table, with the selected one's card. */
export default function TrackDbPage({ selected, onSelect }: { selected: string; onSelect: (uid: string) => void }) {
  const { toast } = useToast()
  const [rows, setRows] = useState<TrackRow[] | null>(null)
  const [query, setQuery] = useState('')
  const [state, setState] = useState<string | null>(null)
  const [domain, setDomain] = useState<string | null>(null)
  const [affiliation, setAffiliation] = useState<string | null>(null)
  const [source, setSource] = useState<string | null>(null)
  const [editing, setEditing] = useState(false)
  // Bumped after a card save, so the card view reloads its values.
  const [cardVersion, setCardVersion] = useState(0)

  useEffect(() => {
    let cancelled = false
    let warned = false
    const load = () =>
      api.tracks().then(
        (r) => {
          if (!cancelled) setRows(r.tracks)
          warned = false
        },
        (e) => {
          if (!cancelled && !warned) toast({ variant: 'error', title: 'Tracks unavailable', message: errorMessage(e) })
          warned = true
        },
      )
    load()
    const t = setInterval(load, REFRESH_MS)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [toast])

  const all = useMemo(() => rows ?? [], [rows])
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase()
    return all.filter((t) => {
      if (state && t.state !== state) return false
      if (domain && t.domain !== domain) return false
      if (affiliation && t.affiliation !== affiliation) return false
      if (source && !t.sources.some((s) => s.startsWith(`${source}/`))) return false
      if (!q) return true
      const hay = [t.track_id, t.name, t.callsign, t.gold_name, t.class, t.sidc.code, ...t.sources, ...(t.identifiers ?? []).map((i) => `${i.scheme}:${i.value}`)]
      return hay.some((h) => h && h.toLowerCase().includes(q))
    })
  }, [all, query, state, domain, affiliation, source])

  const points: MapPoint[] = useMemo(
    () =>
      shown.map((t) => ({
        id: t.uid,
        latitude: t.latitude,
        longitude: t.longitude,
        color: affiliationColor(t.affiliation),
        label: `${t.track_id} ${displayName(t)}`.trim(),
      })),
    [shown],
  )

  const row = all.find((t) => t.uid === selected) ?? null
  const select = (uid: string | null) => {
    setEditing(false)
    onSelect(uid ?? '')
  }
  const filtered = !!(query.trim() || state || domain || affiliation || source)

  return (
    <div className="panels">
      <div className="workspace">
        <CollapsiblePanel title="Track map" badge={rows ? `${points.length.toLocaleString()} shown` : undefined} persistKey="ot.panel.trackmap">
          <div className="workspace-body map">
            <MapView aria-label="Live tracks" points={points} selectedId={selected || null} onSelect={select} outlines={OUTLINES} fitKey="tracks" height="100%" />
          </div>
        </CollapsiblePanel>
        <CollapsiblePanel
          title="Track card"
          persistKey="ot.panel.trackcard"
          actions={
            selected && (
              <Button size="sm" variant="secondary" icon={<TbPencil />} onClick={() => setEditing(true)}>
                Edit
              </Button>
            )
          }
        >
          <div className="workspace-body">
            {selected ? (
              <TrackCard key={selected} uid={selected} cardVersion={cardVersion} />
            ) : (
              <div className="panel-body">
                <span className="muted">Select a track on the map or in the table to see its card.</span>
              </div>
            )}
          </div>
        </CollapsiblePanel>
      </div>

      <CollapsiblePanel
        title="Tracks"
        badge={rows ? (filtered ? `${shown.length.toLocaleString()} of ${all.length.toLocaleString()}` : all.length.toLocaleString()) : undefined}
        persistKey="ot.panel.tracktable"
        titleActions={
          <div className="search">
            <TbSearch aria-hidden />
            <Input
              aria-label="Search tracks"
              placeholder="Search"
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
            <Filter label="State" any="any state" values={distinct(all, (t) => [t.state])} value={state} onChange={setState} />
            <Filter label="Domain" any="any domain" values={distinct(all, (t) => [t.domain])} value={domain} onChange={setDomain} />
            <Filter label="Affiliation" any="any affiliation" values={distinct(all, (t) => [t.affiliation])} value={affiliation} onChange={setAffiliation} />
            <Filter label="Source" any="any source" values={distinct(all, (t) => t.sources.map((s) => s.split('/')[0]))} value={source} onChange={setSource} />
          </>
        }
      >
        <DataTable
          aria-label="Tracks"
          columns={COLUMNS}
          rows={shown}
          rowKey={(t) => t.uid}
          selectedKey={selected || null}
          onRowClick={(t) => select(t.uid)}
          defaultSort={{ key: 'last', direction: 'desc' }}
          maxHeight={480}
          empty={rows === null ? 'LOADING…' : filtered ? 'No track matches.' : 'No live tracks. Enable a source to start ingesting.'}
        />
      </CollapsiblePanel>

      {selected && (
        <CardEditor
          key={`${selected}:${row?.entity_id ?? ''}`}
          entityId={row?.entity_id}
          fromTrack={selected}
          title={row ? displayName(row) || row.track_id : selected}
          open={editing}
          onClose={() => setEditing(false)}
          onSaved={() => setCardVersion((v) => v + 1)}
        />
      )}
    </div>
  )
}
