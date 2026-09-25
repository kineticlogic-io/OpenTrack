import { lazy, Suspense, useEffect, useMemo, useState } from 'react'
import { TbAlertTriangle } from 'react-icons/tb'
import { Badge, DataTable, TabPanel, Tabs, type DataTableColumn } from 'staresdk'
import { api, type ExtensionField, type GraphEdge, type SystemTrack, type TrackResponse } from '../../api/client'
import { ago, errorMessage, fmtNum, fmtTime, show, STATE_COLOR } from '../../lib/format'
import { standardName, symbolUrl } from '../../lib/symbol'

// React Flow loads only when the Provenance tab is open.
const LineageGraph = lazy(() => import('./LineageGraph'))

const REFRESH_MS = 5000

const TABS = [
  { id: 'card', label: 'Card' },
  { id: 'provenance', label: 'Provenance' },
]

type Contributor = SystemTrack['contributors'][number]

const CONTRIBUTOR_COLUMNS: DataTableColumn<Contributor>[] = [
  { key: 'track', header: 'Source track', mono: true, render: (c) => `${c.source_id}/${c.source_track_key}` },
  { key: 'pairing', header: 'Pairing', width: 80, render: (c) => c.pairing },
  { key: 'conf', header: 'Confidence', width: 90, align: 'right', render: (c) => c.confidence.toFixed(2) },
  { key: 'last', header: 'Last report', width: 90, align: 'right', render: (c) => ago(c.last_report) },
]

/** Where a published attribute's value came from. */
function origin(f: ExtensionField, card: Record<string, unknown> | null): string {
  if (f.builtin) return 'OpenTrack'
  if (card && card[f.key] !== undefined && card[f.key] !== null) return 'card'
  return 'feed'
}

/**
 * Render with `key={uid}`, so switching tracks starts from a clean state.
 *
 * A system track's baseball card: the GOLD fields every published track carries (with its
 * MIL-STD-2525 symbol), then the output schema's attributes; and its provenance, the source
 * tracks and correlation decisions behind it.
 */
export function TrackCard({ uid, cardVersion }: { uid: string; cardVersion: number }) {
  const [tab, setTab] = useState('card')
  const [data, setData] = useState<TrackResponse | null>(null)
  const [missing, setMissing] = useState<string | null>(null)
  const [edges, setEdges] = useState<GraphEdge[] | null>(null)
  const [fields, setFields] = useState<ExtensionField[]>([])
  const [card, setCard] = useState<Record<string, unknown> | null>(null)

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
  }, [uid, tab])

  // The schema's fields, and the card's own values to tell card from feed.
  const entityId = data?.track.entity_id ?? null
  useEffect(() => {
    let cancelled = false
    if (entityId) {
      api.card(entityId).then(
        (v) => !cancelled && (setFields(v.schema.fields), setCard(v.card?.values ?? {})),
        () => {},
      )
    } else {
      api.schema().then(
        (o) => !cancelled && setFields(o.versions.find((v) => v.version === o.latest_published)?.fields ?? []),
        () => {},
      )
    }
    return () => {
      cancelled = true
    }
  }, [entityId, cardVersion])

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
  const icon = symbolUrl(m.sidc, 36)
  const attrs = t.attributes ?? {}
  const cardValues = entityId ? card : null
  return (
    <div className="panel-body">
      <div className="card-head">
        {icon && <img src={icon} alt={`${m.affiliation} ${m.domain} symbol`} height={36} />}
        <div className="card-name">
          <strong>{m.name !== 'UNKNOWN' ? m.name : (t.view.callsign ?? m.name)}</strong>
          <span className="mono muted">{m.track_id}</span>
        </div>
        <span className="spacer" />
        {t.published === false && (
          <span title="Kept inside OpenTrack: only a sensor that may not stand alone reports for it, or it is not confirmed yet.">
            <Badge color="grey" size="sm" uppercase>
              not published
            </Badge>
          </span>
        )}
        <Badge color={STATE_COLOR[t.state] ?? 'grey'} size="sm" uppercase>
          {t.state}
        </Badge>
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
            </dl>
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
                          <span className="muted">{origin(f, cardValues)}</span>
                        </>
                      )}
                      {(notices[f.key] ?? []).map((n) => (
                        <span key={n.source_id} className="notice">
                          <TbAlertTriangle aria-hidden /> {n.source_id} reports <span className="mono">{show(n.feed)}</span>;
                          the card value is published.
                        </span>
                      ))}
                    </dd>
                  </div>
                ))}
              </dl>
            )}
            {!entityId && <span className="muted">No card yet. Edit to start one.</span>}
          </div>
        ) : (
          <div className="stack">
            <h3 className="subhead">Source tracks</h3>
            <DataTable
              aria-label="Source tracks"
              columns={CONTRIBUTOR_COLUMNS}
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
