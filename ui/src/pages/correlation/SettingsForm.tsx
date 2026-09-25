import { FieldSelect, Input, Label } from 'staresdk'
import type { CorrelationSettings } from '../../api/client'
import { INPUT } from '../../lib/valueSpec'

const APPROACHES = [{ name: 'kinematics_metadata' }, { name: 'kinematics' }, { name: 'identifiers' }]
const MODES = [{ name: 'automatic' }, { name: 'suggest' }]
const YES_NO = [{ name: 'yes' }, { name: 'no' }]

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

function Num({ label, value, onChange, step, width = 90 }: { label: string; value: number; onChange: (v: number) => void; step?: number; width?: number }) {
  return (
    <Input
      style={{ ...INPUT, width }}
      type="number"
      step={step ?? 'any'}
      aria-label={label}
      value={Number.isFinite(value) ? String(value) : ''}
      onChange={(e) => onChange(e.target.value === '' ? NaN : Number(e.target.value))}
    />
  )
}

/** Correlation settings: how tracks pair, whether pairings are made or proposed, and splits. */
export function SettingsForm({ value, onChange }: { value: CorrelationSettings; onChange: (v: CorrelationSettings) => void }) {
  const k = value.kinematic
  const sp = value.split
  const setK = (patch: Partial<CorrelationSettings['kinematic']>) => onChange({ ...value, kinematic: { ...k, ...patch } })
  const setS = (patch: Partial<CorrelationSettings['split']>) => onChange({ ...value, split: { ...sp, ...patch } })
  return (
    <div className="stack">
      <Row label="Pair on" hint="Shared identifiers always pair. Kinematics: agreeing motion too; with metadata, vetoed by conflicting identifiers or domains.">
        <FieldSelect
          ariaLabel="Pairing approach"
          fields={APPROACHES}
          value={value.approach}
          onChange={(v) => onChange({ ...value, approach: (v ?? 'kinematics_metadata') as CorrelationSettings['approach'] })}
          style={{ width: 200 }}
        />
      </Row>
      <Row label="Mode" hint="Suggest: kinematic pairings wait for an operator to accept them.">
        <FieldSelect
          ariaLabel="Pairing mode"
          fields={MODES}
          value={value.mode}
          onChange={(v) => onChange({ ...value, mode: v === 'suggest' ? 'suggest' : 'automatic' })}
          style={{ width: 200 }}
        />
      </Row>
      <h4 className="subhead">Kinematic pairing</h4>
      <Row label="Pair after" hint={`${k.m} of the last ${k.n} comparisons agree, within ${k.window_secs} s.`}>
        <div className="num-row">
          <Num width={58} label="Agreeing comparisons" value={k.m} step={1} onChange={(m) => setK({ m })} />
          <span className="muted">of</span>
          <Num width={58} label="Comparisons" value={k.n} step={1} onChange={(n) => setK({ n })} />
          <span className="muted">within</span>
          <Num width={58} label="Window seconds" value={k.window_secs} onChange={(window_secs) => setK({ window_secs })} />
          <span className="muted">s</span>
        </div>
      </Row>
      <Row label="Agree within" hint="Chi-square on distance ÷ both uncertainties (9.21: 99% of true matches).">
        <Num label="Pairing gate" value={k.chi2_gate} onChange={(chi2_gate) => setK({ chi2_gate })} />
      </Row>
      <Row label="Position σ at least (m)">
        <Num label="Minimum sigma" value={k.min_sigma_m} onChange={(min_sigma_m) => setK({ min_sigma_m })} />
      </Row>
      <Row label="Drift (m/s)" hint="How fast uncertainty grows while a view is dead-reckoned.">
        <Num label="Drift" value={k.drift_mps} onChange={(drift_mps) => setK({ drift_mps })} />
      </Row>
      <Row label="Compare views up to (s)">
        <Num label="Maximum view age" value={k.max_age_secs} onChange={(max_age_secs) => setK({ max_age_secs })} />
      </Row>
      <h4 className="subhead">Identifier pairing</h4>
      <Row label="Sanity gate (m)" hint="Plus the domain's top speed × time apart; a farther identifier match is refused.">
        <Num label="Sanity gate" value={value.gate.base_m} onChange={(base_m) => onChange({ ...value, gate: { ...value.gate, base_m } })} />
      </Row>
      <h4 className="subhead">Best source</h4>
      <Row label="Freshness (s)" hint="Reports this close to the newest compete on accuracy for the position.">
        <Num label="Freshness" value={value.freshness_secs} onChange={(freshness_secs) => onChange({ ...value, freshness_secs })} />
      </Row>
      <h4 className="subhead">Splits</h4>
      <Row label="Propose splits" hint="When a source track stops agreeing with the rest of its track.">
        <FieldSelect ariaLabel="Propose splits" fields={YES_NO} value={sp.propose ? 'yes' : 'no'} onChange={(v) => setS({ propose: v === 'yes' })} style={{ width: 90 }} />
      </Row>
      <Row label="Split without asking">
        <FieldSelect ariaLabel="Split automatically" fields={YES_NO} value={sp.automatic ? 'yes' : 'no'} onChange={(v) => setS({ automatic: v === 'yes' })} style={{ width: 90 }} />
      </Row>
      <Row label="Disagree beyond" hint={`${sp.m} of the last ${sp.n} comparisons beyond this chi-square (18.4: 99.99%).`}>
        <div className="num-row">
          <Num width={58} label="Split gate" value={sp.chi2_gate} onChange={(chi2_gate) => setS({ chi2_gate })} />
          <Num width={58} label="Disagreeing comparisons" value={sp.m} step={1} onChange={(m) => setS({ m })} />
          <span className="muted">of</span>
          <Num width={58} label="Split comparisons" value={sp.n} step={1} onChange={(n) => setS({ n })} />
        </div>
      </Row>
    </div>
  )
}
