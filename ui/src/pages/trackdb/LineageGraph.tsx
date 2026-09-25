import { useMemo } from 'react'
import '@xyflow/react/dist/base.css'
import { GraphView, type GraphViewEdge, type GraphViewNode } from 'staresdk/graph-view'
import type { GraphEdge } from '../../api/client'

const UID_RE = /^[A-Z0-9]{3}\d{9}$/

/** A track's lineage from its graph edges: the source tracks reporting for it (current and
 *  past), system tracks merged into or split from it, and the card it resolves to. */
export default function LineageGraph({ uid, edges, entityId }: { uid: string; edges: GraphEdge[]; entityId?: string }) {
  const graph = useMemo(() => {
    const nodes = new Map<string, GraphViewNode>()
    const add = (key: string) => {
      if (nodes.has(key)) return
      const system = UID_RE.test(key)
      nodes.set(key, {
        id: key,
        label: system ? `tms-${key}` : key,
        sublabel: system ? 'system track' : 'source track',
        tone: key === uid ? 'accent' : 'neutral',
      })
    }
    const out: GraphViewEdge[] = []
    for (const e of edges) {
      add(e.src_key)
      add(e.dst_key)
      out.push({
        id: String(e.id),
        source: e.src_key,
        target: e.dst_key,
        label: e.kind,
        ended: e.valid_to_ms !== null,
      })
    }
    // A source track whose every link has ended no longer reports for anything.
    for (const n of nodes.values()) {
      if (n.id !== uid && out.filter((e) => e.source === n.id).every((e) => e.ended)) n.tone = 'muted'
    }
    if (entityId) {
      nodes.set(`card:${entityId}`, { id: `card:${entityId}`, label: entityId, sublabel: 'card' })
      out.push({ id: 'card', source: uid, target: `card:${entityId}`, label: 'CARD' })
    }
    return { nodes: [...nodes.values()], edges: out }
  }, [uid, edges, entityId])

  return <GraphView aria-label="Track lineage" nodes={graph.nodes} edges={graph.edges} height={260} />
}
