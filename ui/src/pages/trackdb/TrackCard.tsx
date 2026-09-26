import { lazy, Suspense, useEffect, useMemo, useState } from 'react'
import { TbAlertTriangle, TbArrowsSplit, TbPencil, TbUnlink } from 'react-icons/tb'
import { Badge, Button, DataTable, TabPanel, Tabs, useToast, type DataTableColumn } from 'staresdk'
import { api, type ExtensionField, type GraphEdge, type SystemTrack, type TrackResponse } from '../../api/client'
import { ago, errorMessage, fmtNum, fmtTime, show, STATE_COLOR } from '../../lib/format'
import { standardName, symbolUrl } from '../../lib/symbol'
import { useCan } from '../../auth/context'

// React Flow loads only when the Provenance tab is open.
const LineageGraph = lazy(() => import('./LineageGraph'))

const REFRESH_MS = 5000

const TABS = [
  { id: 'card', label: 'Details' },
  { id: 'provenance', label: 'Provenance' },
]

type Contributor = SystemTrack['contributors'][number]

/** Plots a detection source's contributions are keyed under (see the engine). */
const DETECTIONS = '~detections'
const pct = (p: number) => `${Math.round(p * 1000) / 10}%`
const keyOf = (c: Contributor) => `${c.source_id}/${c.source_track_key}`

/** How a source track came to report for this track, from its live link in the graph. */
function pairedBy(edge: GraphEdge | undefined): string {
  if (!edge) return '—'
  const evidence = (edge.attrs.evidence ?? {}) as Record<string, unknown>
  if (evidence.rule === 'identifier') return `shared ${String(evidence.identity ?? 'identifier')}`
  switch (edge.decision_op) {
    case 'create_system_track':
      return 'started this track'
    case 'pair':
      return 'shared identifier'
    case 'merge': {
      // "a and b agreed kinematically in 4 of 4 comparisons (6 m apart, σ 9 m)" → the evidence part.
      const why = edge.decision_reason?.match(/(agreed kinematically.*|share .*)$/)?.[1]
      return why ? `merged: ${why}` : `merged (decision #${edge.decision_id})`
    }
    case 'split':
      return 'split off another track'
    default:
      return edge.decision_op
  }
}

/** Where a published attribute's value came from, where that is known. */
function origin(f: ExtensionField, replaced: boolean): string {
  if (f.builtin) return 'OpenTrack'
  return replaced ? 'entity' : ''
}

/**
 * Render with `key={uid}`, so switching tracks starts from a clean state.
 *
 * A system track's baseball card: the GOLD fields every published track carries (with its
 * MIL-STD-2525 symbol), then the output schema's attributes; and its provenance, the source
 * tracks and correlation decisions behind it.
 */
export function TrackCard({ uid, onEdit, editLabel = 'Edit' }: { uid: string; onEdit?: () => void; editLabel?: string }) {
  const [tab, setTab] = useState('card')
  const canManage = useCan('track_manager')
  const [data, setData] = useState<TrackResponse | null>(null)
  const [missing, setMissing] = useState<string | null>(null)
  const [edges, setEdges] = useState<GraphEdge[] | null>(null)
  const [fields, setFields] = useState<ExtensionField[]>([])
  const [edgesRev, setEdgesRev] = useState(0)
  const [splitting, setSplitting] = useState<string | null>(null)
  const { toast, confirm } = useToast()

  // Live state, refreshed.
  useEffect(() => {
    let cancelled = false
    const load = () =>
      api.track(uid).then(
        (r) => !cancelled && (setData(r), setMissing(null)),
        (e) => !cancelled && setMissing(errorMessage(e)),
      )
    load()
    const t = setInterval(load, REFRESH_MS)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [uid])

  // Graph history, when provenance is shown.
  useEffect(() => {
    if (tab !== 'provenance') return
    let cancelled = false
    api.explain(uid).then(
      (r) => !cancelled && setEdges(r.edges),
      () => !cancelled && setEdges([]),
    )
    return () => {
      cancelled = true
    }
  }, [uid, tab, edgesRev])

  // The latest published output schema's fields.
  const entityId = data?.track.entity_id ?? null
  useEffect(() => {
    let cancelled = false
    api.schema().then(
      (o) => !cancelled && setFields(o.versions.find((v) => v.version === o.latest_published)?.fields ?? []),
      () => {},
    )
    return () => {
      cancelled = true
    }
  }, [])

  const notices = useMemo(() => {
    const out: Record<string, NonNullable<SystemTrack['notices']>> = {}
    for (const n of data?.track.notices ?? []) (out[n.key] ??= []).push(n)
    return out
  }, [data])

  if (!data) {
    return (
      <div className="panel-body">
        {missing ? <span className="muted">{missing}. The track may have been retired.</span> : <span className="muted">LOADING…</span>}
      </div>
    )
  }
  const t = data.track
  const m = data.message
  const own = t.contributors.filter((c) => c.source_track_key !== DETECTIONS)
  const live = new Map((edges ?? []).filter((e) => e.kind === 'REPORTS_FOR' && e.valid_to_ms === null).map((e) => [e.src_key, e]))
  const split = async (c: Contributor) => {
    const key = keyOf(c)
    const ok = await confirm(`${key} leaves this track for a track of its own, and the two are not paired again.`, {
      title: 'Split source track',
      confirmLabel: 'Split',
    })
    if (!ok) return
    setSplitting(key)
    try {
      const r = await api.splitTrack(t.uid, key)
      toast({ variant: 'success', title: 'Split', message: `${key} is now ${r.new_track}` })
      setData({ ...data, track: { ...t, contributors: t.contributors.filter((x) => keyOf(x) !== key) } })
      setEdgesRev((n) => n + 1)
    } catch (e) {
      toast({ variant: 'error', title: 'Not split', message: errorMessage(e) })
    } finally {
      setSplitting(null)
    }
  }
  const contributorColumns: DataTableColumn<Contributor>[] = [
    { key: 'track', header: 'Source track', mono: true, render: keyOf },
    {
      key: 'by',
      header: 'Paired by',
      render: (c) => {
        const text = c.source_track_key === DETECTIONS ? 'plots associated' : pairedBy(live.get(keyOf(c)))
        return <span title={text}>{text}</span>
      },
    },
    {
      key: 'conf',
      header: 'Confidence',
      width: 96,
      align: 'right',
      render: (c) => {
        if (c.source_track_key === DETECTIONS) return '—'
        const exists = c.existence != null ? `, exists ${pct(c.existence)}` : ''
        return (
          <span title={`Same object as the rest: ${pct(c.confidence)}${exists}`}>
            {pct(c.confidence)}
            {c.existence != null && <span className="muted"> · {pct(c.existence)}</span>}
          </span>
        )
      },
    },
    { key: 'last', header: 'Last', width: 56, align: 'right', render: (c) => ago(c.last_report) },
    {
      key: 'act',
      header: '',
      width: 84,
      align: 'right',
      render: (c) =>
        own.length > 1 && c.source_track_key !== DETECTIONS ? (
          <Button size="sm" variant="ghost" icon={<TbArrowsSplit />} disabled={!canManage || splitting !== null} onClick={() => split(c)} title="Split it off this track">
            Split
          </Button>
        ) : null,
    },
  ]
  const icon = symbolUrl(m.sidc, 36)
  const attrs = t.attributes ?? {}
  return (
    <div className="panel-body">
      <div className="card-head">
        {icon && <img src={icon} alt={`${m.affiliation} ${m.domain} symbol`} height={36} />}
        <div className="card-name">
          <strong>{m.name !== 'UNKNOWN' ? m.name : (t.view.callsign ?? m.name)}</strong>
          <span className="mono muted">{m.track_id}</span>
        </div>
        <span className="spacer" />
        {t.filtered ? (
          <span title={`Held back by the output filter: ${t.filtered}`}>
            <Badge color="warning" size="sm" uppercase>
              filtered
            </Badge>
          </span>
        ) : (
          t.published === false && (
            <span title="Kept inside OpenTrack: only sensors that may not stand alone report for it, or it is not confirmed yet.">
              <Badge color="grey" size="sm" uppercase>
                not published
              </Badge>
            </span>
          )
        )}
        <Badge color={STATE_COLOR[t.state] ?? 'grey'} size="sm" uppercase>
          {t.state}
        </Badge>
        {onEdit && canManage && (
          <Button size="sm" variant="secondary" icon={<TbPencil />} onClick={onEdit}>
            {editLabel}
          </Button>
        )}
      </div>
      <Tabs aria-label="Track card views" idPrefix="trk" size="sm" value={tab} onChange={setTab} tabs={TABS} />
      <TabPanel id={tab} idPrefix="trk">
        {tab === 'card' ? (
          <div className="stack">
            <dl className="facts">
              <dt>Class-name</dt>
              <dd>
                {m.class}-{m.name}
              </dd>
              <dt>Force code</dt>
              <dd className="mono">
                {String(m.force_code).padStart(2, '0')} · {m.domain} {m.affiliation}
              </dd>
              <dt>Track type</dt>
              <dd className="mono">{m.track_type}</dd>
              <dt>SIDC</dt>
              <dd className="mono">
                {m.sidc.code} <span className="muted">({standardName(m.sidc.standard)})</span>
              </dd>
              <dt>Time</dt>
              <dd className="mono">{fmtTime(m.time)}</dd>
              <dt>Position</dt>
              <dd className="mono">
                {m.lat.toFixed(5)}, {m.lon.toFixed(5)}
              </dd>
              <dt>Course / speed</dt>
              <dd className="mono">
                {fmtNum(t.view.kinematics.course_deg, 0, '°')} / {fmtNum(t.view.kinematics.speed_mps, 1, ' m/s')}
              </dd>
              <dt>Subject</dt>
              <dd className="mono">{data.subject}</dd>
              <dt>Entity</dt>
              <dd className="mono">{entityId ?? <span className="muted">none</span>}</dd>
              {(t.members ?? []).length > 0 && (
                <>
                  <dt>Members</dt>
                  <dd className="mono">{(t.members ?? []).map((u) => `tms-${u}`).join(', ')}</dd>
                </>
              )}
              {(t.groups ?? []).length > 0 && (
                <>
                  <dt>Groups</dt>
                  <dd className="mono">{(t.groups ?? []).join(', ')}</dd>
                </>
              )}
              {(t.paired_with ?? []).length > 0 && (
                <>
                  <dt>Paired with</dt>
                  <dd>
                    {(t.paired_with ?? []).map((p) => (
                      <span key={p} className="num-row">
                        <span className="mono">tms-{p}</span>
                        <Button
                          size="xs"
                          variant="ghost"
                          icon={<TbUnlink />}
                          aria-label={`Unpair tms-${p}`}
                          disabled={!canManage}
                          title="Unpair"
                          onClick={async () => {
                            try {
                              await api.unpairTracks(m.track_id, `tms-${p}`)
                              toast({ variant: 'success', title: 'Unpaired', message: `tms-${p}` })
                            } catch (e) {
                              toast({ variant: 'error', title: 'Not unpaired', message: errorMessage(e) })
                            }
                          }}
                        />
                      </span>
                    ))}
                  </dd>
                </>
              )}
            </dl>
            {(t.notices ?? []).length > 0 && (
              <div className="stack" style={{ gap: 4 }}>
                {(t.notices ?? []).map((n) => (
                  <span key={`${n.key}:${n.source_id}`} className="notice">
                    <TbAlertTriangle aria-hidden /> <span className="mono">{n.key}</span>: {n.source_id} reports{' '}
                    <span className="mono">{show(n.feed)}</span>; the entity&apos;s <span className="mono">{show(n.entity)}</span> is published.
                  </span>
                ))}
              </div>
            )}
            <h3 className="subhead">Attributes</h3>
            {fields.length === 0 ? (
              <span className="muted">The output schema has no attributes. Add fields in the Schema workspace.</span>
            ) : (
              <dl className="facts">
                {fields.map((f) => (
                  <div key={f.key} style={{ display: 'contents' }}>
                    <dt className="mono">{f.key}</dt>
                    <dd>
                      {attrs[f.key] === undefined ? (
                        <span className="muted">—</span>
                      ) : (
                        <>
                          <span className="mono">{show(attrs[f.key])}</span>{' '}
                          <span className="muted">{origin(f, (notices[`ext.${f.key}`] ?? []).length > 0)}</span>
                        </>
                      )}
                    </dd>
                  </div>
                ))}
              </dl>
            )}
          </div>
        ) : (
          <div className="stack">
            <h3 className="subhead">
              Source tracks
              {data.confidence != null && (
                <span className="muted" title="Probability that the track is a real object, from its sources' existence and pairing confidences">
                  {' '}
                  · confidence {pct(data.confidence)}
                </span>
              )}
            </h3>
            <DataTable
              aria-label="Source tracks"
              columns={contributorColumns}
              rows={t.contributors}
              rowKey={(c) => `${c.source_id}/${c.source_track_key}`}
              empty="No source track reports for this track."
            />
            <h3 className="subhead">Lineage</h3>
            {edges === null ? (
              <span className="muted">LOADING…</span>
            ) : edges.length === 0 ? (
              <span className="muted">No graph history.</span>
            ) : (
              <>
                <Suspense fallback={<span className="muted">LOADING…</span>}>
                  <LineageGraph uid={t.uid} edges={edges} entityId={t.entity_id} />
                </Suspense>
                <h3 className="subhead">Correlation decisions</h3>
                <ul className="history">
                  {edges.map((e) => (
                    <li key={e.id} className={e.valid_to_ms === null ? '' : 'ended'}>
                      <Badge color={e.valid_to_ms === null ? 'blue' : 'grey'} size="sm">
                        {e.kind}
                      </Badge>
                      <span className="mono">
                        {e.src_key} → {e.dst_key}
                      </span>
                      <span />
                      <span className="muted">
                        {fmtTime(e.valid_from_ms)}
                        {e.valid_to_ms !== null && ` – ${fmtTime(e.valid_to_ms)}`} · decision #{e.decision_id} (
                        {e.decision_op} by {e.decision_actor})
                      </span>
                    </li>
                  ))}
                </ul>
              </>
            )}
            <dl className="facts">
              <dt>First seen</dt>
              <dd className="mono">{fmtTime(t.first_seen)}</dd>
              <dt>Last seen</dt>
              <dd className="mono">{fmtTime(t.last_seen)}</dd>
              <dt>Observations</dt>
              <dd className="mono">{t.observation_count.toLocaleString()}</dd>
            </dl>
          </div>
        )}
      </TabPanel>
    </div>
  )
}
