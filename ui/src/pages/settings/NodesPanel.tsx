import { useEffect, useState } from 'react'
import { Badge, CollapsiblePanel, DataTable, Input, SaveButton, Toggle, type DataTableColumn } from 'staresdk'
import { api, DEFAULT_SYNC, type SyncSettings, type SyncStatus } from '../../api/client'
import { InfoTip } from '../../components/InfoTip'
import { fmtTime } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'
import { SettingsRow as Row } from './SettingsRow'

const REFRESH_MS = 5_000
/** A peer not heard for longer than this is shown as silent. */
const SILENT_MS = 30_000

interface PeerRow {
  site: string
  heard: string | null
  seq: number | null
}

/**
 * Sharing the picture with other OpenTrack nodes (docs/multi-node.md): which nodes this one trusts, whether it
 * accepts track management, and how the link is going. The settings are part of the page's draft.
 */
export function NodesPanel({
  value,
  onChange,
  siteCode,
  dirty,
  saving,
  saved,
  onSave,
}: {
  value: SyncSettings | undefined
  onChange: (s: SyncSettings) => void
  siteCode: string
  dirty: boolean
  saving: boolean
  saved: boolean
  onSave: () => void
}) {
  const sync = value ?? DEFAULT_SYNC
  const set = (patch: Partial<SyncSettings>) => onChange({ ...sync, ...patch })
  const [peersText, setPeersText] = useState(sync.peers.join(', '))
  const [status, setStatus] = useState<SyncStatus | null>(null)
  useEffect(() => setPeersText(sync.peers.join(', ')), [sync.peers])
  useEffect(() => {
    const load = () => api.syncStatus().then(setStatus, () => {})
    load()
    const t = setInterval(load, REFRESH_MS)
    return () => clearInterval(t)
  }, [])

  const heads = new Map((status?.log.heads ?? []).map((h) => [h.site, h.seq]))
  const heard = status?.link?.heard ?? {}
  const rows: PeerRow[] = sync.peers.map((site) => ({ site, heard: heard[site] ?? null, seq: heads.get(site) ?? null }))
  const columns: DataTableColumn<PeerRow>[] = [
    { key: 'site', header: 'Node', width: 90, mono: true, render: (r) => r.site },
    {
      key: 'state',
      header: 'Link',
      width: 110,
      render: (r) => {
        const live = r.heard != null && Date.now() - Date.parse(r.heard) < SILENT_MS
        return (
          <Badge color={live ? 'success' : r.heard == null ? 'grey' : 'warning'} size="sm">
            {r.heard == null ? 'never heard' : live ? 'heard' : 'silent'}
          </Badge>
        )
      },
    },
    { key: 'heard', header: 'Last heard', width: 180, mono: true, render: (r) => (r.heard ? fmtTime(Date.parse(r.heard)) : '—') },
    { key: 'seq', header: 'Its decisions held', mono: true, render: (r) => r.seq ?? 0 },
  ]
  const byStatus = status?.log.by_status ?? {}
  const e = status?.engine

  return (
    <CollapsiblePanel title="Nodes" persistKey="ot.panel.settings.nodes" actions={<SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={onSave} />}>
      <div className="panel-body stack">
        <Row label="Share the picture" hint="Exchange tracks and track-management decisions with the nodes below, through this node's NATS (ot.sync.*), which a networking package or opentrack bridge carries. Every node gets the whole picture under one set of track numbers; each reports only what its own sensors see.">
          <Toggle value={sync.enabled} onChange={(enabled) => set({ enabled })} aria-label="Share the picture" />
        </Row>
        <Row label="Trusted nodes" hint={`Site codes of the nodes this one exchanges with, comma separated (this node is ${siteCode}). Messages from any other node are dropped.`}>
          <Input
            style={{ ...INPUT, width: 320 }}
            aria-label="Trusted nodes"
            placeholder="e.g. AAA, BBB"
            value={peersText}
            onChange={(ev) => setPeersText(ev.target.value)}
            onBlur={() =>
              set({
                peers: peersText
                  .split(/[\s,]+/)
                  .map((p) => p.trim().toUpperCase())
                  .filter(Boolean),
              })
            }
          />
        </Row>
        <Row label="Receive only" hint="Apply other nodes' track management here, but accept none from this node's own users: for a node with no operator, such as a drone.">
          <Toggle value={sync.receive_only} onChange={(receive_only) => set({ receive_only })} aria-label="Receive only" />
        </Row>
        <Row label="Share the profile" hint="Publishing an output schema or saving correlation settings on any node applies on every node that shares the profile (the later change wins), so every node publishes the same attributes and pairs by the same rules.">
          <Toggle value={sync.share_profile} onChange={(share_profile) => set({ share_profile })} aria-label="Share the profile" />
        </Row>
        {sync.peers.length > 0 && (
          <DataTable aria-label="Trusted nodes" rows={rows} columns={columns} rowKey={(r) => r.site} density="compact" />
        )}
        <div className="num-row muted">
          <span>This node reports {e?.reporting ?? 0} tracks to the others and holds {e?.from_other_nodes ?? 0} only they see.</span>
          <span>
            Decisions: {byStatus.applied ?? 0} applied, {byStatus.pending ?? 0} waiting for their tracks, {byStatus.superseded ?? 0} superseded,{' '}
            {byStatus.failed ?? 0} failed.
          </span>
          <InfoTip label="Decisions">
            Track management from every node, under ids like AAA:12. One naming a track this node has not heard of yet waits for it (12 h at most). One that
            conflicts with a later decision on the same tracks is kept but changes nothing.
          </InfoTip>
          {status?.link === undefined || status?.link === null ? <Badge color="grey" size="sm">no link running</Badge> : null}
        </div>
      </div>
    </CollapsiblePanel>
  )
}
