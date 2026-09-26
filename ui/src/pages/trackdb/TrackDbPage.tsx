import { useEffect, useMemo, useState } from 'react'
import { TbArrowMerge, TbLink, TbSearch, TbTrash, TbUsersGroup, TbX } from 'react-icons/tb'
import { setWorkerUrl } from 'maplibre-gl'
import maplibreWorkerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url'
import { Badge, Button, CollapsiblePanel, DataTable, FieldSelect, Input, Modal, useToast, type DataTableColumn } from 'staresdk'
import { MapView, type MapPoint } from 'staresdk/map-view'
import { api, type Entity, type TrackRow } from '../../api/client'
import { ago, errorMessage, fmtNum, STATE_COLOR } from '../../lib/format'
import { affiliationColor } from '../../lib/palette'
import { symbolUrl } from '../../lib/symbol'
import { EntityEditor } from '../registry/EntityEditor'
import { InfoTip } from '../../components/InfoTip'
import { GroupEditor } from './GroupEditor'
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
  {
    key: 'links',
    header: 'Group / pair',
    width: 110,
    render: (t) =>
      t.kind === 'group' ? (
        <Badge color="blue" size="sm" title={`${t.members?.length ?? 0} members`}>
          group · {t.members?.length ?? 0}
        </Badge>
      ) : (
        <span className="num-row">
          {(t.groups ?? []).length > 0 && (
            <Badge color="grey" size="sm" title={`Member of ${(t.groups ?? []).join(', ')}`}>
              in group
            </Badge>
          )}
          {(t.paired_with ?? []).length > 0 && (
            <Badge color="grey" size="sm" title={`Paired with ${(t.paired_with ?? []).join(', ')}`}>
              paired
            </Badge>
          )}
        </span>
      ),
    sortValue: (t) => (t.kind === 'group' ? 2 : (t.groups ?? []).length + (t.paired_with ?? []).length > 0 ? 1 : 0),
  },
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
    key: 'entity',
    header: 'Entity',
    width: 100,
    render: (t) =>
      t.notices > 0 ? (
        <Badge color="warning" size="sm" title="The entity replaced values a feed reports">
          replaced {t.notices}
        </Badge>
      ) : t.entity_id ? (
        <Badge color="blue" size="sm">
          entity
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

/** Every live track on a map and in a table, with the selected one's details and entity. */
export default function TrackDbPage({ selected, onSelect }: { selected: string; onSelect: (uid: string) => void }) {
  const { toast } = useToast()
  const [rows, setRows] = useState<TrackRow[] | null>(null)
  const [query, setQuery] = useState('')
  const [state, setState] = useState<string | null>(null)
  const [domain, setDomain] = useState<string | null>(null)
  const [affiliation, setAffiliation] = useState<string | null>(null)
  const [source, setSource] = useState<string | null>(null)
  const [editing, setEditing] = useState(false)
  // Tracks ticked for track management, by track id, in the order ticked.
  const [checked, setChecked] = useState<string[]>([])
  const [merging, setMerging] = useState<string | null>(null)
  const [grouping, setGrouping] = useState<{ id: string | null; members: string[] } | null>(null)
  const [busy, setBusy] = useState(false)
  const { confirm } = useToast()

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
  const byId = useMemo(() => new Map(all.map((t) => [t.track_id, t])), [all])
  const ticked = checked.filter((id) => byId.has(id))
  const tickedRows = ticked.map((id) => byId.get(id)!)
  const tickedGroups = tickedRows.filter((t) => t.kind === 'group')
  const tickedTracks = tickedRows.filter((t) => t.kind !== 'group')
  const toggle = (id: string) => setChecked((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id]))
  const columns: DataTableColumn<TrackRow>[] = useMemo(
    () => [
      {
        key: 'tick',
        header: '',
        width: 34,
        render: (t) => (
          <input
            type="checkbox"
            className="tick"
            aria-label={`Select ${t.track_id}`}
            checked={checked.includes(t.track_id)}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => e.stopPropagation()}
            onChange={() => toggle(t.track_id)}
          />
        ),
      },
      ...COLUMNS,
    ],
    [checked],
  )
  const reload = () => api.tracks().then((r) => setRows(r.tracks), () => {})
  const run = async (what: string, f: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await f()
      setChecked([])
      await reload()
    } catch (e) {
      toast({ variant: 'error', title: `Not ${what}`, message: errorMessage(e) })
    } finally {
      setBusy(false)
    }
  }
  const label = (t: TrackRow) => `${displayName(t) || t.track_id}`
  const pair = () =>
    run('paired', async () => {
      await api.pairTracks(ticked)
      toast({ variant: 'success', title: 'Paired', message: tickedRows.map(label).join(', ') })
    })
  const merge = (into: string) =>
    run('merged', async () => {
      for (const from of ticked.filter((id) => id !== into)) await api.mergeTracks(from, into, true)
      setMerging(null)
      toast({ variant: 'success', title: 'Merged', message: `into ${label(byId.get(into)!)}` })
    })
  const remove = async () => {
    const ok = await confirm(
      `Delete ${ticked.length} track${ticked.length === 1 ? '' : 's'}? ${tickedGroups.length ? 'Groups are dissolved (their members stay). ' : ''}They are deleted downstream; a source still reporting starts a new track.`,
      { title: 'Delete tracks', confirmLabel: 'Delete' },
    )
    if (ok) await run('deleted', () => api.deleteTracks(ticked))
  }
  const group = () => {
    if (tickedGroups.length === 1 && tickedTracks.length > 0) {
      const g = tickedGroups[0]
      void run('added', async () => {
        await api.groupMembers(g.track_id, tickedTracks.map((t) => t.track_id), [])
        toast({ variant: 'success', title: 'Added to group', message: `${tickedTracks.length} to ${label(g)}` })
      })
    } else if (tickedGroups.length === 1) {
      setGrouping({ id: tickedGroups[0].track_id, members: [] })
    } else {
      setGrouping({ id: null, members: tickedTracks.map((t) => t.track_id) })
    }
  }
  const select = (uid: string | null) => {
    setEditing(false)
    onSelect(uid ?? '')
  }
  const filtered = !!(query.trim() || state || domain || affiliation || source)

  return (
    <div className="panels tight">
      <div className="workspace">
        <CollapsiblePanel title="Track map" badge={rows ? `${points.length.toLocaleString()} shown` : undefined} persistKey="ot.panel.trackmap">
          <div className="workspace-body map">
            <MapView aria-label="Live tracks" points={points} selectedId={selected || null} onSelect={select} outlines={OUTLINES} fitKey="tracks" height="100%" />
          </div>
        </CollapsiblePanel>
        <CollapsiblePanel
          title="Track card"
          persistKey="ot.panel.trackcard"
        >
          <div className="workspace-body">
            {selected ? (
              <TrackCard
                key={selected}
                uid={selected}
                onEdit={
                  row
                    ? () => (row.kind === 'group' ? setGrouping({ id: row.track_id, members: [] }) : setEditing(true))
                    : undefined
                }
                editLabel={row?.kind === 'group' ? 'Edit group' : row?.entity_id ? 'Edit entity' : 'Edit'}
              />
            ) : (
              <div className="panel-body">
                <span className="muted">Select a track on the map or in the table to see its details.</span>
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
        <div className="selection-bar">
          {ticked.length === 0 ? (
            <>
              <span className="muted">Tick tracks to pair, merge, group or delete them.</span>
              <InfoTip label="Track management">
                Pair: the same object, kept as separate tracks. Merge: one track survives with the others&apos; history and sources, and
                correlation never splits it again. Group: a battle group, flight or convoy published as a track of its own at its members&apos;
                centre. Delete: the track is deleted downstream.
              </InfoTip>
              <span className="spacer" />
              <Button size="sm" variant="ghost" disabled={shown.length === 0} onClick={() => setChecked(shown.slice(0, 200).map((t) => t.track_id))}>
                Select shown
              </Button>
            </>
          ) : (
            <>
              <strong>{ticked.length} selected</strong>
              <Button
                size="sm"
                variant="ghost"
                icon={<TbLink />}
                disabled={busy || tickedTracks.length < 2 || tickedGroups.length > 0}
                title="The same object: keep the tracks separate, each listing the others"
                onClick={pair}
              >
                Pair
              </Button>
              <Button
                size="sm"
                variant="ghost"
                icon={<TbArrowMerge />}
                disabled={busy || tickedTracks.length < 2 || tickedGroups.length > 0}
                title="One track survives with the others' history and sources"
                onClick={() => setMerging(ticked[0])}
              >
                Merge
              </Button>
              <Button
                size="sm"
                variant="ghost"
                icon={<TbUsersGroup />}
                disabled={busy || tickedGroups.length > 1}
                title={tickedGroups.length === 1 ? (tickedTracks.length ? 'Add the ticked tracks to the ticked group' : 'Edit the group') : 'Form a group of the ticked tracks'}
                onClick={group}
              >
                {tickedGroups.length === 1 ? (tickedTracks.length ? 'Add to group' : 'Edit group') : 'Group'}
              </Button>
              <Button size="sm" variant="ghost" icon={<TbTrash />} disabled={busy} onClick={remove}>
                Delete
              </Button>
              <span className="spacer" />
              <Button size="sm" variant="ghost" icon={<TbX />} onClick={() => setChecked([])}>
                Clear
              </Button>
            </>
          )}
        </div>
        <DataTable
          aria-label="Tracks"
          columns={columns}
          rows={shown}
          rowKey={(t) => t.uid}
          selectedKey={selected || null}
          onRowClick={(t) => select(t.uid)}
          defaultSort={{ key: 'last', direction: 'desc' }}
          maxHeight={480}
          empty={rows === null ? 'LOADING…' : filtered ? 'No track matches.' : 'No live tracks. Enable a source to start ingesting.'}
        />
      </CollapsiblePanel>

      {merging !== null && (
        <Modal title="Merge tracks" onClose={() => setMerging(null)} width={480} resizable={false}>
          <div className="panel-body">
            <span className="muted">
              The surviving track keeps its number and takes the others&apos; history, sources, groups and pairings; the others are deleted
              downstream. Correlation never splits the merged track.
            </span>
            <div className="stack" style={{ gap: 6 }} role="radiogroup" aria-label="Surviving track">
              {tickedTracks.map((t) => (
                <label key={t.track_id} className="radio-row">
                  <input type="radio" name="survivor" checked={merging === t.track_id} onChange={() => setMerging(t.track_id)} />
                  <span className="mono">{t.track_id}</span>
                  <span>{displayName(t)}</span>
                  {t.published === false && <span className="muted">not published</span>}
                </label>
              ))}
            </div>
            <div className="num-row" style={{ justifyContent: 'flex-end' }}>
              <Button size="sm" variant="ghost" onClick={() => setMerging(null)}>
                Cancel
              </Button>
              <Button size="sm" icon={<TbArrowMerge />} disabled={busy} onClick={() => merge(merging)}>
                Merge into {merging}
              </Button>
            </div>
          </div>
        </Modal>
      )}

      {grouping && (
        <GroupEditor
          groupId={grouping.id}
          members={grouping.members}
          rows={all}
          open
          onClose={() => setGrouping(null)}
          onSaved={(id) => {
            setChecked([])
            void reload()
            if (id) setGrouping({ id, members: [] })
          }}
        />
      )}

      {selected && row && row.kind !== 'group' && (
        <EntityEditor
          entityId={row.entity_id}
          seed={{
            name: displayName(row) || null,
            domain: (row.domain !== 'unknown' ? row.domain : null) as Entity['domain'],
            affiliation: (row.affiliation !== 'unknown' ? row.affiliation : null) as Entity['affiliation'],
            // A track no identifier names (radar, GMTI): the entity is pinned to this track.
            identifiers: (row.identifiers ?? []).length
              ? (row.identifiers ?? []).map((i) => ({ ...i, expected_name: row.name ?? null }))
              : [{ scheme: 'track', value: row.track_id }],
          }}
          open={editing}
          onClose={() => setEditing(false)}
          onSaved={() => {}}
        />
      )}
    </div>
  )
}
