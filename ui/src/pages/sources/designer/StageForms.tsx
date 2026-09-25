import { TbPlus, TbTrash } from 'react-icons/tb'
import { Button, FieldSelect, Input, Label, Toggle } from 'staresdk'
import type { SecurityLabel } from '../../../api/client'
import { describeCondition, describeValue, type ValueSpec } from '../../../lib/pipeline'
import { JsonField } from './JsonField'
import { INPUT } from '../../../lib/valueSpec'
import { ValueEditor } from './ValueEditor'

type Obj = Record<string, unknown>
type Props = { value: Obj; onChange: (v: Obj) => void }

const words = (s: string) =>
  s
    .split(/[\s,]+/)
    .map((x) => x.trim())
    .filter(Boolean)
const num = (s: string) => (s.trim() === '' ? undefined : Number(s))

function Row({ label, children, hint }: { label: string; children: React.ReactNode; hint?: string }) {
  return (
    <div className="stage-row">
      <Label size="sm">{label}</Label>
      <div className="stack" style={{ gap: 2 }}>
        {children}
        {hint && <span className="muted">{hint}</span>}
      </div>
    </div>
  )
}

const CODECS = ['json', 'cot_xml', 'xml']

/** How frames become records. */
export function DecodeForm({ value, onChange }: Props) {
  const type = String(value.type ?? 'json')
  return (
    <div className="stack">
      <Row label="Format">
        <FieldSelect
          ariaLabel="Codec"
          fields={CODECS.map((name) => ({ name }))}
          value={type}
          onChange={(t) => onChange(t === 'xml' ? { type: t, record_element: '' } : { type: t ?? 'json' })}
          style={{ width: 160 }}
        />
      </Row>
      {type === 'json' && (
        <>
          <Row label="Records at" hint="Path to the array of records in each frame (adsb.lol: ac). Empty: the frame is the record, or each element of an array.">
            <Input
              style={INPUT}
              aria-label="Records path"
              value={String(value.records ?? '')}
              onChange={(e) => onChange({ ...value, records: e.target.value || undefined })}
              spellCheck={false}
            />
          </Row>
          <Row label="Frame fields" hint="Frame-level fields copied into every record under _frame (e.g. now), comma-separated.">
            <Input
              style={INPUT}
              aria-label="Frame context fields"
              value={((value.context as string[]) ?? []).join(', ')}
              onChange={(e) => {
                const c = words(e.target.value)
                onChange({ ...value, context: c.length ? c : undefined })
              }}
              spellCheck={false}
            />
          </Row>
        </>
      )}
      {type === 'xml' && (
        <Row label="Record element" hint="One record per element with this name.">
          <Input style={INPUT} aria-label="Record element" value={String(value.record_element ?? '')} onChange={(e) => onChange({ ...value, record_element: e.target.value })} spellCheck={false} />
        </Row>
      )}
      {type === 'cot_xml' && <span className="muted">One record per Cursor-on-Target event.</span>}
    </div>
  )
}

/** Records dropped before mapping, each with a reason (counted in the source's metrics). */
export function RejectForm({ rules, onChange }: { rules: Obj[]; onChange: (r: Obj[]) => void }) {
  return (
    <div className="stack">
      {rules.map((r, i) => (
        <div key={i} className="map-field">
          <div className="value-row">
            <Label size="sm">Reason</Label>
            <Input style={{ ...INPUT, width: 200 }} aria-label={`Reject rule ${i + 1} reason`} value={String(r.reason ?? '')} onChange={(e) => onChange(rules.map((x, j) => (j === i ? { ...x, reason: e.target.value } : x)))} />
            <span className="spacer" />
            <Button size="xs" variant="ghost" icon={<TbTrash />} aria-label={`Remove reject rule ${i + 1}`} title="Remove" onClick={() => onChange(rules.filter((_, j) => j !== i))} />
          </div>
          <JsonField
            label={`Reject rule ${i + 1} condition`}
            optional
            value={r.when}
            placeholder='{"path": "lat", "exists": false}'
            onChange={(v) => onChange(rules.map((x, j) => (j === i ? { ...x, when: v } : x)))}
            describe={(v) => `drop when ${describeCondition(v)}`}
          />
        </div>
      ))}
      <div>
        <Button size="sm" variant="secondary" icon={<TbPlus />} onClick={() => onChange([...rules, { reason: 'reason', when: { path: '', exists: false } }])}>
          Reject rule
        </Button>
      </div>
    </div>
  )
}

export function JoinForm({ value, onChange }: Props) {
  const hours = Number(value.ttl_secs ?? 7 * 86400) / 3600
  return (
    <Row label="Keep identity for" hint="Hours a track's static fields stay usable after its last static report.">
      <Input style={{ ...INPUT, width: 120 }} type="number" aria-label="Identity kept for hours" value={String(hours)} onChange={(e) => onChange({ ttl_secs: Math.round(Number(e.target.value || 0) * 3600) })} />
    </Row>
  )
}

const GRADES = ['exact', 'hull', 'name', 'generic', 'stale']

export function RegistryForm({ value, onChange }: Props) {
  const apply = Object.entries((value.apply as Obj) ?? {})
  const setApply = (entries: [string, unknown][]) => onChange({ ...value, apply: Object.fromEntries(entries) })
  return (
    <div className="stack">
      <Row label="Schemes" hint="Identifier schemes to resolve, in priority order. Empty: every identifier.">
        <Input style={INPUT} aria-label="Registry schemes" value={((value.schemes as string[]) ?? []).join(', ')} onChange={(e) => onChange({ ...value, schemes: words(e.target.value) })} spellCheck={false} />
      </Row>
      <Row label="Name to grade" hint="The observation field compared with the entity's name.">
        <Input style={INPUT} aria-label="Broadcast name field" value={String(value.broadcast_name ?? 'name')} onChange={(e) => onChange({ ...value, broadcast_name: e.target.value })} spellCheck={false} />
      </Row>
      <Row label="Apply at grades" hint={`Grades at which the fields below overwrite the feed: ${GRADES.join(', ')}.`}>
        <Input
          style={INPUT}
          aria-label="Apply grades"
          value={((value.apply_grades as string[]) ?? ['exact', 'hull', 'name', 'generic']).join(', ')}
          onChange={(e) => onChange({ ...value, apply_grades: words(e.target.value) })}
          spellCheck={false}
        />
      </Row>
      <h4 className="subhead">Fields set from the entity</h4>
      {apply.map(([to, from], i) => (
        <div key={i} className="value-row">
          <Input style={{ ...INPUT, width: 220 }} aria-label={`Apply ${i + 1} observation field`} placeholder="classification.cot_type" value={to} onChange={(e) => setApply(apply.map(([k, v], j) => (j === i ? [e.target.value, v] : [k, v])))} spellCheck={false} />
          <span className="muted">← entity field</span>
          <Input style={{ ...INPUT, width: 160 }} aria-label={`Apply ${i + 1} entity field`} placeholder="cot, name, flag…" value={String(from)} onChange={(e) => setApply(apply.map(([k, v], j) => (j === i ? [k, e.target.value] : [k, v])))} spellCheck={false} />
          <Button size="xs" variant="ghost" icon={<TbTrash />} aria-label={`Remove apply ${i + 1}`} title="Remove" onClick={() => setApply(apply.filter((_, j) => j !== i))} />
        </div>
      ))}
      <div>
        <Button size="sm" variant="secondary" icon={<TbPlus />} disabled={apply.some(([k]) => k === '')} onClick={() => setApply([...apply, ['', '']])}>
          Field
        </Button>
      </div>
    </div>
  )
}

const AFFILIATIONS = ['friend', 'hostile', 'neutral', 'unknown', 'suspect', 'assumed_friend', 'pending']

export function AffiliationForm({ value, onChange }: Props) {
  const listInput = (key: string, label: string) => (
    <Row label={label}>
      <Input style={INPUT} aria-label={`${label} countries`} placeholder="ISO codes: US GB FR" value={((value[key] as string[]) ?? []).join(' ')} onChange={(e) => onChange({ ...value, [key]: words(e.target.value.toUpperCase()) })} spellCheck={false} />
    </Row>
  )
  const pick = (key: string, label: string) => (
    <Row label={label}>
      <FieldSelect ariaLabel={label} allowNone fields={AFFILIATIONS.map((name) => ({ name }))} value={(value[key] as string) ?? null} onChange={(v) => onChange({ ...value, [key]: v ?? undefined })} style={{ width: 160 }} />
    </Row>
  )
  return (
    <div className="stack">
      <Row label="Country from" hint={`Currently: ${describeValue(value.country as ValueSpec)}`}>
        <ValueEditor label="Country" value={value.country as ValueSpec} onChange={(v) => onChange({ ...value, country: v })} />
      </Row>
      {listInput('friendly', 'Friend')}
      {listInput('hostile', 'Hostile')}
      {listInput('neutral', 'Neutral')}
      {pick('otherwise', 'Other countries')}
      {pick('unknown_country', 'No country')}
    </div>
  )
}

export function FilterForm({ value, onChange }: Props) {
  return (
    <div className="stack">
      <Row label="Keep only if" hint="Conditions over the observation (after mapping), e.g. {&quot;path&quot;: &quot;classification.domain&quot;, &quot;eq&quot;: &quot;surface&quot;}.">
        <JsonField label="Keep condition" optional value={value.keep_if} onChange={(v) => onChange({ ...value, keep_if: v })} describe={(v) => `keep when ${describeCondition(v)}`} />
      </Row>
      <Row label="Drop if">
        <JsonField label="Drop condition" optional value={value.drop_if} onChange={(v) => onChange({ ...value, drop_if: v })} describe={(v) => `drop when ${describeCondition(v)}`} />
      </Row>
    </div>
  )
}

const ALGORITHMS = [{ name: 'gnn' }, { name: 'mht' }]
const DOMAINS = ['surface', 'ground', 'air', 'subsurface']

/** Turn detections into tracks. Tracks leave with no identity and unknown affiliation. */
export function TrackerForm({ value, onChange }: Props) {
  const mht = (value.mht as Obj | undefined) ?? {}
  const field = (key: string, label: string, fallback: number, obj: Obj = value, set = (v: Obj) => onChange(v)) => (
    <Row label={label}>
      <Input style={{ ...INPUT, width: 120 }} type="number" aria-label={label} value={String(obj[key] ?? fallback)} onChange={(e) => set({ ...obj, [key]: num(e.target.value) })} />
    </Row>
  )
  const setMht = (m: Obj) => onChange({ ...value, mht: m })
  const auto = (value.auto_timing as Obj | undefined) ?? {}
  const setAuto = (a: Obj) => onChange({ ...value, auto_timing: a })
  return (
    <div className="stack">
      <span className="muted">Tracks get unknown affiliation and at most a domain, never an identity or type.</span>
      <Row label="Algorithm" hint={value.algorithm === 'mht' ? 'Multiple hypotheses: fewer false tracks in clutter, more work.' : 'Global nearest neighbour: the best plot-to-track assignment each scan.'}>
        <FieldSelect
          ariaLabel="Tracker algorithm"
          fields={ALGORITHMS}
          value={String(value.algorithm ?? 'gnn')}
          onChange={(a) => onChange({ ...value, algorithm: a ?? 'gnn' })}
          style={{ width: 160 }}
        />
      </Row>
      <Row label="Domain" hint="Everything this sensor sees. Empty: what most of a track's plots report, if any.">
        <FieldSelect ariaLabel="Tracker domain" allowNone fields={DOMAINS.map((name) => ({ name }))} value={(value.domain as string) ?? null} onChange={(d) => onChange({ ...value, domain: d ?? undefined })} style={{ width: 160 }} />
      </Row>
      {field('measurement_sigma_m', 'Plot error σ (m)', 10)}
      {field('process_noise_mps2', 'Manoeuvre (m/s²)', 0.5)}
      {field('cluster_m', 'Merge plots within (m)', 0)}
      <h4 className="subhead">Existence</h4>
      <span className="muted">
        Each track carries the probability that it is a real target: plots raise it by how well they fit against clutter, looks without one lower it.
        It is published as the track&apos;s confidence.
      </span>
      {field('detection_probability', 'Detection probability', 0.9)}
      {field('clutter_density', 'False plots per m²', 1e-6)}
      {field('birth_density', 'New targets per m²', 1e-7)}
      {field('confirm_probability', 'Confirm at probability', 0.95)}
      {field('drop_probability', 'Drop at probability', 0.02)}
      {field('target_lifetime_secs', 'Target lifetime (s)', 600)}
      {field('confirm_hits', 'Confirm after at least (plots)', 3)}
      <Row label="Auto timing" hint="Size the drop windows to the sensor's revisit rate, as the codec measures it (STANAG 4607), and count a miss once per revisit instead of once per scan.">
        <Toggle
          size="sm"
          aria-label="Auto timing"
          value={value.auto_timing != null}
          onChange={(on) => onChange({ ...value, auto_timing: on ? {} : undefined })}
        />
      </Row>
      {value.auto_timing != null ? (
        <>
          {field('drop_tentative_revisits', 'Drop unconfirmed after (revisits)', 1.3, auto, setAuto)}
          {field('drop_confirmed_revisits', 'Drop after (revisits)', 2.5, auto, setAuto)}
          {field('min_drop_tentative_secs', '…drop unconfirmed after at least (s)', 8, auto, setAuto)}
          {field('min_drop_confirmed_secs', '…drop after at least (s)', 30, auto, setAuto)}
        </>
      ) : (
        <>
          {field('drop_tentative_secs', 'Drop unconfirmed after (s) without a plot', 3)}
          {field('drop_confirmed_secs', 'Drop after (s) without a plot', 8)}
        </>
      )}
      <Row label="Scans" hint="A scan is one frame, or the plots with the same time.">
        <FieldSelect
          ariaLabel="Scan grouping"
          fields={[{ name: 'frame' }, { name: 'time' }]}
          value={String(value.scans ?? 'frame')}
          onChange={(v) => onChange({ ...value, scans: v === 'time' ? 'time' : undefined })}
          style={{ width: 160 }}
        />
      </Row>
      <Row label="Track keys">
        <Input style={{ ...INPUT, width: 120 }} aria-label="Track key prefix" value={String(value.key_prefix ?? 'T')} onChange={(e) => onChange({ ...value, key_prefix: e.target.value || undefined })} spellCheck={false} />
      </Row>
      {value.algorithm === 'mht' && (
        <>
          <h4 className="subhead">Hypotheses</h4>
          {field('n_scan', 'Final after scans', 3, mht, setMht)}
          {field('max_branches', 'Hypotheses per target', 20, mht, setMht)}
        </>
      )}
    </div>
  )
}

/** What the source's observations are, which decides how correlation takes them. */
export function PublishForm({
  reports,
  tracker,
  publishAlone,
  onChange,
  onPublishAlone,
}: {
  reports?: string
  tracker: boolean
  publishAlone?: boolean
  onChange: (r: 'detections' | undefined) => void
  onPublishAlone: (v: boolean | undefined) => void
}) {
  const alone = publishAlone ?? reports !== 'detections'
  return (
    <div className="stack">
      <Row label="The feed reports">
        <FieldSelect
          ariaLabel="The feed reports"
          fields={[{ name: 'tracks' }, { name: 'detections' }]}
          value={reports ?? 'tracks'}
          onChange={(v) => onChange(v === 'detections' ? 'detections' : undefined)}
          style={{ width: 160 }}
        />
      </Row>
      <span className="muted">
        {tracker
          ? 'The tracker stage turns the plots into tracks; correlation pairs them with other sources.'
          : reports === 'detections'
            ? 'Each plot updates the nearest system track another source keeps; plots near none are dropped. Add a Tracker stage to form tracks instead.'
            : 'Every observation that passes reaches correlation.'}
      </span>
      <Row label="Publish its lone tracks" hint="No: its tracks stay inside OpenTrack until a source that may stand alone reports for them too.">
        <FieldSelect
          ariaLabel="Publish lone tracks"
          fields={[{ name: 'yes' }, { name: 'no' }]}
          value={alone ? 'yes' : 'no'}
          onChange={(v) => {
            const want = v === 'yes'
            onPublishAlone(want === (reports !== 'detections') ? undefined : want)
          }}
          style={{ width: 160 }}
        />
      </Row>
    </div>
  )
}

export function ThrottleForm({ value, onChange }: Props) {
  const field = (key: string, label: string, hint: string, fallback: number) => (
    <Row label={label} hint={hint}>
      <Input style={{ ...INPUT, width: 120 }} type="number" aria-label={label} value={String(value[key] ?? fallback)} onChange={(e) => onChange({ ...value, [key]: num(e.target.value) })} />
    </Row>
  )
  return (
    <div className="stack">
      {field('min_interval_secs', 'At most every (s)', 'Never more often than this per source track.', 0)}
      {field('min_move_m', 'Or after moving (m)', 'Between that and the heartbeat, only when the track moved this far.', 0)}
      {field('heartbeat_secs', 'Heartbeat (s)', 'Always pass a report after this long.', 600)}
    </div>
  )
}

/** An optional security label on everything the source reports, in the fields OpenStare's ICD
 *  reserves (`security.classification`, `security.restrictions`, `security.sharing`). Free text. */
export function SecurityForm({ value, onChange }: { value?: SecurityLabel; onChange: (v: SecurityLabel | undefined) => void }) {
  const on = value !== undefined
  const v = value ?? { classification: '' }
  const set = (patch: Partial<SecurityLabel>) => onChange({ ...v, ...patch })
  return (
    <div className="stack">
      <h4 className="subhead">Security label</h4>
      <span className="muted">
        Label everything this source reports: its tracks carry it into OpenTrack&apos;s output as <span className="mono">security</span>. A track several
        labelled sources report for takes the label of the highest-priority one.
      </span>
      <Row label="Label this source">
        <Toggle size="sm" aria-label="Label this source" value={on} onChange={(yes) => onChange(yes ? { classification: '' } : undefined)} />
      </Row>
      {on && (
        <>
          <Row label="Classification" hint="security.classification, e.g. SECRET">
            <Input style={{ ...INPUT, width: 320 }} aria-label="Classification" value={v.classification} maxLength={256} onChange={(e) => set({ classification: e.target.value })} />
          </Row>
          <Row label="Restrictions" hint="security.restrictions: comma separated, e.g. NOFORN, ORCON">
            <Input
              style={{ ...INPUT, width: 320 }}
              aria-label="Restrictions"
              defaultValue={(v.restrictions ?? []).join(', ')}
              onBlur={(e) => {
                const r = e.target.value.split(',').map((x) => x.trim()).filter(Boolean)
                set({ restrictions: r.length ? r : undefined })
              }}
            />
          </Row>
          <Row label="Sharing" hint="security.sharing: who the classification may be released to, e.g. REL TO USA, FVEY">
            <Input style={{ ...INPUT, width: 320 }} aria-label="Sharing" value={v.sharing ?? ''} maxLength={256} onChange={(e) => set({ sharing: e.target.value || undefined })} />
          </Row>
        </>
      )}
    </div>
  )
}
