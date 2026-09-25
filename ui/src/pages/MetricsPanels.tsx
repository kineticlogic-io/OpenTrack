import { useEffect, useMemo, useState } from 'react'
import 'uplot/dist/uPlot.min.css'
import { CollapsiblePanel, useToast } from 'staresdk'
import { TimeSeriesChart, type ChartSeries } from 'staresdk/chart'
import { api, type SystemMetrics } from '../api/client'
import { errorMessage } from '../lib/format'

const REFRESH_MS = 15_000
const MINUTES = 60

type Point = SystemMetrics['series'][number]

const MB = 1024 * 1024
const mb = (v: number | undefined | null) => (v === undefined || v === null ? null : Math.round((v / MB) * 10) / 10)
const fmtMb = (v: number | null | undefined) => (v === null || v === undefined ? '—' : `${(v / MB).toFixed(1)} MB`)

function fmtUptime(s: number): string {
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`
}

/** A counter per minute (0 when nothing was counted). */
const count = (pts: Point[], pick: (p: Point) => number | undefined) => pts.map((p) => pick(p) ?? 0)
/** A gauge per minute (a gap before the sampler recorded one). */
const gauge = (pts: Point[], pick: (p: Point) => number | undefined | null) => pts.map((p) => pick(p) ?? null)
const sum = (m: Record<string, number>, keys: string[]) => keys.reduce((n, k) => n + (m[k] ?? 0), 0)

function Stat({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="stat">
      <span className="stat-value">{typeof value === 'number' ? value.toLocaleString() : value}</span>
      <span className="stat-label">{label}</span>
    </div>
  )
}

function Chart({ title, series, times, unit }: { title: string; series: ChartSeries[]; times: number[]; unit?: string }) {
  return (
    <div className="chart">
      <h3 className="subhead">{title}</h3>
      <TimeSeriesChart aria-label={title} times={times} series={series} unit={unit} height={130} />
    </div>
  )
}

/** Throughput, tracks, backlog and resources over the last hour, per minute. */
export default function MetricsPanels() {
  const { toast } = useToast()
  const [m, setM] = useState<SystemMetrics | null>(null)

  useEffect(() => {
    let cancelled = false
    let warned = false
    const load = () =>
      api.systemMetrics(MINUTES).then(
        (r) => {
          if (!cancelled) setM(r)
          warned = false
        },
        (e) => {
          // One toast per outage, not one per refresh.
          if (!cancelled && !warned) toast({ variant: 'error', title: 'Metrics unavailable', message: errorMessage(e) })
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

  // The current minute is still filling; charts end at the last complete one.
  const pts = useMemo(() => (m ? m.series.slice(0, -1) : []), [m])
  const times = useMemo(() => pts.map((p) => p.minute * 60), [pts])
  const sourceIds = useMemo(() => [...new Set(pts.flatMap((p) => Object.keys(p.emitted_by_source)))].sort(), [pts])

  if (!m) {
    return (
      <CollapsiblePanel title="Throughput" persistKey="ot.panel.throughput">
        <div className="panel-body">
          <span className="muted">LOADING…</span>
        </div>
      </CollapsiblePanel>
    )
  }
  const l = m.live
  const state = (k: string) => l.by_state[k] ?? 0

  return (
    <>
      <CollapsiblePanel title="Throughput" badge="per minute · last hour" persistKey="ot.panel.throughput">
        <div className="panel-body">
          <div className="charts">
            <Chart
              title="Ingest"
              times={times}
              series={[
                { label: 'frames', values: count(pts, (p) => p.ingest.frames) },
                { label: 'observations', values: count(pts, (p) => p.ingest.emitted), tone: 'info' },
                {
                  label: 'dropped',
                  values: count(pts, (p) => sum(p.ingest, ['rejected', 'unmatched', 'filtered', 'throttled'])),
                  tone: 'neutral',
                },
                { label: 'errors', values: count(pts, (p) => sum(p.ingest, ['decode_error', 'invalid'])), tone: 'danger' },
              ]}
            />
            <Chart
              title="Observations by source"
              times={times}
              series={sourceIds.map((id) => ({ label: id, values: count(pts, (p) => p.emitted_by_source[id]) }))}
            />
            <Chart
              title="Correlation"
              times={times}
              series={[
                { label: 'applied', values: count(pts, (p) => p.engine.observations) },
                { label: 'new tracks', values: count(pts, (p) => p.engine.created), tone: 'info' },
                { label: 'paired', values: count(pts, (p) => (p.engine.paired ?? 0) + (p.engine.merged ?? 0)), tone: 'success' },
                { label: 'lost', values: count(pts, (p) => p.engine.lost), tone: 'warning' },
                { label: 'dropped', values: count(pts, (p) => p.engine.dropped), tone: 'neutral' },
              ]}
            />
            <Chart
              title="Output"
              times={times}
              series={[
                { label: 'tracks written', values: count(pts, (p) => p.writer.written) },
                { label: 'deletes', values: count(pts, (p) => p.writer.deleted), tone: 'neutral' },
                { label: 'raw feed', values: count(pts, (p) => p.ingest.raw_published), tone: 'info' },
                { label: 'errors', values: count(pts, (p) => sum(p.writer, ['write_error']) + (p.ingest.raw_error ?? 0)), tone: 'danger' },
              ]}
            />
          </div>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel title="Tracks" badge={`${l.tracks.toLocaleString()} live`} persistKey="ot.panel.trackstats">
        <div className="panel-body">
          <div className="stats">
            <Stat label="Live" value={l.tracks} />
            <Stat label="Confirmed" value={state('confirmed')} />
            <Stat label="Tentative" value={state('tentative')} />
            <Stat label="Lost" value={state('lost')} />
            <Stat label="With a card" value={l.with_card} />
            <Stat label="Card differs" value={l.notices} />
            {Object.entries(l.by_domain)
              .sort((a, b) => b[1] - a[1])
              .map(([d, n]) => (
                <Stat key={d} label={d} value={n} />
              ))}
          </div>
          <div className="charts">
            <Chart
              title="Live tracks"
              times={times}
              series={[
                { label: 'live', values: gauge(pts, (p) => p.system.tracks) },
                { label: 'confirmed', values: gauge(pts, (p) => p.system.tracks_confirmed), tone: 'info' },
                { label: 'lost', values: gauge(pts, (p) => p.system.tracks_lost), tone: 'warning' },
                { label: 'with a card', values: gauge(pts, (p) => p.system.tracks_with_card), tone: 'neutral' },
              ]}
            />
          </div>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel title="Backlog and resources" persistKey="ot.panel.resources">
        <div className="panel-body">
          <div className="stats">
            <Stat label="CPU" value={l.cpu_milli === null ? '—' : `${(l.cpu_milli / 10).toFixed(1)}%`} />
            <Stat label="Memory" value={fmtMb(l.rss_bytes)} />
            <Stat label="Threads" value={l.threads} />
            <Stat label="Redis" value={fmtMb(l.redis_bytes)} />
            <Stat label="SQLite" value={fmtMb(l.sqlite_bytes)} />
            <Stat label="NATS stream" value={fmtMb(l.nats_bytes)} />
            <Stat label="Engine backlog" value={l.observations.lag + l.observations.pending} />
            <Stat label="Writer backlog" value={l.outbox.lag + l.outbox.pending} />
            <Stat label="Uptime" value={fmtUptime(l.uptime_secs)} />
          </div>
          <div className="charts">
            <Chart
              title="Backlog"
              times={times}
              series={[
                { label: 'engine waiting', values: gauge(pts, (p) => p.system.obs_lag) },
                { label: 'engine in hand', values: gauge(pts, (p) => p.system.obs_pending), tone: 'neutral' },
                { label: 'writer waiting', values: gauge(pts, (p) => p.system.outbox_lag), tone: 'info' },
                { label: 'writer in hand', values: gauge(pts, (p) => p.system.outbox_pending), tone: 'warning' },
              ]}
            />
            <Chart
              title="CPU"
              unit="%"
              times={times}
              series={[
                {
                  label: 'of one core',
                  values: gauge(pts, (p) => (p.system.cpu_milli === undefined ? null : p.system.cpu_milli / 10)),
                },
              ]}
            />
            <Chart
              title="Memory and storage"
              unit=" MB"
              times={times}
              series={[
                { label: 'OpenTrack', values: gauge(pts, (p) => mb(p.system.rss_bytes)) },
                { label: 'Redis', values: gauge(pts, (p) => mb(p.system.redis_bytes)), tone: 'info' },
                { label: 'SQLite', values: gauge(pts, (p) => mb(p.system.sqlite_bytes)), tone: 'neutral' },
                { label: 'NATS stream', values: gauge(pts, (p) => mb(p.system.nats_bytes)), tone: 'warning' },
              ]}
            />
          </div>
        </div>
      </CollapsiblePanel>
    </>
  )
}
