import { useState } from 'react'
import { TbAlertTriangle, TbPlus, TbTrash } from 'react-icons/tb'
import { Badge, Button, FieldSelect, Input, Label, Tabs } from 'staresdk'
import type { SchemaOverview } from '../../../api/client'
import { describeCondition, destinations, type ValueSpec } from '../../../lib/pipeline'
import { JsonField } from './JsonField'
import { INPUT } from '../../../lib/valueSpec'
import { ValueEditor } from './ValueEditor'

type Obj = Record<string, unknown>
const OTHER_EXT = 'other ext field…'
const KINDS = ['observation', 'static']

/** Where a target is published, as badges and warnings. */
export function Published({ target, schema }: { target: string; schema: SchemaOverview }) {
  if (!target) return <span className="muted">pick a target</span>
  return (
    <span className="dest">
      {destinations(target, schema).map((d) =>
        d.kind === 'attribute' ? (
          <Badge key={d.text} color="blue" size="sm">
            {d.text}
          </Badge>
        ) : d.kind === 'gold' ? (
          <Badge key={d.text} color="grey" size="sm" title="Always-published OTH-GOLD field">
            GOLD {d.text}
          </Badge>
        ) : d.kind === 'none' ? (
          <span key={d.text} className="notice" style={{ margin: 0 }}>
            <TbAlertTriangle aria-hidden /> {d.text}
          </span>
        ) : (
          <span key={d.text} className="muted">
            {d.text}
          </span>
        ),
      )}
    </span>
  )
}

/** Pick a mapping target: an OpenTrack field, or an `ext.` field of the mapping's schema version. */
function TargetPicker({ value, onChange, options, label }: { value: string; onChange: (t: string) => void; options: string[]; label: string }) {
  const custom = value !== '' && !options.includes(value)
  const [other, setOther] = useState(custom)
  return (
    <div className="value-row">
      <FieldSelect
        ariaLabel={label}
        allowNone
        fields={[...options, OTHER_EXT].map((name) => ({ name }))}
        value={other ? OTHER_EXT : value || null}
        onChange={(v) => {
          if (v === OTHER_EXT) {
            setOther(true)
            onChange(value.startsWith('ext.') ? value : 'ext.')
          } else {
            setOther(false)
            onChange(v ?? '')
          }
        }}
        style={{ width: 230 }}
      />
      {other && (
        <Input style={INPUT} aria-label={`${label}: ext field name`} value={value} onChange={(e) => onChange(e.target.value)} placeholder="ext.my_field" spellCheck={false} />
      )}
    </div>
  )
}

/**
 * The Map stage: rules that turn a decoded record into observation fields. Each rule has a name,
 * a kind (observation, or static identity fields joined onto later reports), an optional
 * condition and bindings, the track key, identifiers, and fields: target ← value.
 */
export function MapEditor({ mapping, onChange, schema }: { mapping: Obj; onChange: (m: Obj) => void; schema: SchemaOverview }) {
  const rules = (mapping.rules as Obj[]) ?? []
  const [active, setActive] = useState(0)
  // Bumped when rows are removed, so editors below them reload their values.
  const [epoch, setEpoch] = useState(0)
  const i = Math.min(active, Math.max(rules.length - 1, 0))
  const rule = rules[i]

  const version = Number(mapping.schema_version ?? 1)
  const extFields = (schema.versions.find((v) => v.version === version)?.fields ?? []).filter((f) => !f.builtin).map((f) => `ext.${f.key}`)
  const targets = [...schema.core, ...extFields]

  const setRule = (patch: Obj) => onChange({ ...mapping, rules: rules.map((r, j) => (j === i ? { ...r, ...patch } : r)) })
  const drop = (o: Obj, k: string) => Object.fromEntries(Object.entries(o).filter(([x]) => x !== k))
  const fields = Object.entries((rule?.fields as Obj) ?? {})
  const setFields = (entries: [string, unknown][]) => setRule({ fields: Object.fromEntries(entries) })
  const identifiers = (rule?.identifiers as Obj[]) ?? []

  return (
    <div className="stack">
      <div className="value-row">
        <Tabs
          aria-label="Mapping rules"
          idPrefix="rule"
          size="sm"
          value={`r${i}`}
          onChange={(id) => setActive(Number(id.slice(1)))}
          tabs={rules.map((r, j) => ({ id: `r${j}`, label: String(r.name || `rule ${j + 1}`) }))}
        />
        <Button
          size="sm"
          variant="secondary"
          icon={<TbPlus />}
          onClick={() => {
            onChange({ ...mapping, rules: [...rules, { name: `rule ${rules.length + 1}`, key: '', fields: {} }] })
            setActive(rules.length)
          }}
        >
          Rule
        </Button>
      </div>
      {!rule ? (
        <span className="muted">No rules: nothing is mapped. Add a rule.</span>
      ) : (
        <div key={`${i}:${epoch}`} className="stack">
          <div className="value-row">
            <Label size="sm">Name</Label>
            <Input style={{ ...INPUT, width: 180 }} aria-label="Rule name" value={String(rule.name ?? '')} onChange={(e) => setRule({ name: e.target.value })} />
            <Label size="sm">Kind</Label>
            <FieldSelect ariaLabel="Rule kind" fields={KINDS.map((name) => ({ name }))} value={String(rule.kind ?? 'observation')} onChange={(v) => setRule({ kind: v === 'static' ? 'static' : undefined })} style={{ width: 130 }} />
            <span className="spacer" />
            <Button
              size="xs"
              variant="ghost"
              icon={<TbTrash />}
              aria-label="Remove rule"
              title="Remove rule"
              onClick={() => {
                onChange({ ...mapping, rules: rules.filter((_, j) => j !== i) })
                setActive(Math.max(0, i - 1))
                setEpoch(epoch + 1)
              }}
            />
          </div>
          <span className="muted">
            {rule.kind === 'static'
              ? 'Static: identity fields (name, type, dimensions…) cached per track key and filled into later position reports.'
              : 'Observation: each matching record becomes a position report.'}
          </span>

          <h4 className="subhead">Applies when</h4>
          <JsonField
            label="Rule condition"
            optional
            value={rule.when}
            placeholder='Every record. Or e.g. {"path": "MessageType", "in": ["PositionReport"]}'
            onChange={(v) => setRule(v === undefined ? drop(rule, 'when') : { when: v })}
            describe={(v) => `only when ${describeCondition(v)}`}
          />
          <h4 className="subhead">Bindings</h4>
          <JsonField
            label="Rule bindings"
            optional
            value={rule.let}
            placeholder='None. Or e.g. {"_body": {"path": "Message", "key": "MessageType"}} to read _body.Name'
            onChange={(v) => setRule(v === undefined ? drop(rule, 'let') : { let: v })}
          />

          <h4 className="subhead">Track key</h4>
          <span className="muted">Identifies the source track; records with the same key update the same track.</span>
          <ValueEditor label="Track key" value={rule.key as ValueSpec} onChange={(v) => setRule({ key: v })} />

          <h4 className="subhead">Identifiers</h4>
          <span className="muted">Registry matching and correlation use these (mmsi, icao, imo, …).</span>
          {identifiers.map((id, j) => (
            <div key={`${j}:${epoch}`} className="map-row">
              <Input style={{ ...INPUT, width: 110 }} aria-label={`Identifier ${j + 1} scheme`} placeholder="scheme" value={String(id.scheme ?? '')} onChange={(e) => setRule({ identifiers: identifiers.map((x, k) => (k === j ? { ...x, scheme: e.target.value } : x)) })} />
              <ValueEditor label={`Identifier ${j + 1}`} value={id.value as ValueSpec} onChange={(v) => setRule({ identifiers: identifiers.map((x, k) => (k === j ? { ...x, value: v } : x)) })} />
              <Button
                size="xs"
                variant="ghost"
                icon={<TbTrash />}
                aria-label={`Remove identifier ${j + 1}`}
                title="Remove identifier"
                onClick={() => {
                  setRule({ identifiers: identifiers.filter((_, k) => k !== j) })
                  setEpoch(epoch + 1)
                }}
              />
            </div>
          ))}
          <div>
            <Button size="sm" variant="secondary" icon={<TbPlus />} onClick={() => setRule({ identifiers: [...identifiers, { scheme: '', value: '' }] })}>
              Identifier
            </Button>
          </div>

          <h4 className="subhead">Fields</h4>
          <span className="muted">
            Each field: the OpenTrack field it sets, where its value comes from, and where that is published. To publish a
            feed value as an attribute, map it to <span className="mono">ext.&lt;field&gt;</span>, or map it to an OpenTrack
            field that a schema field links to.
          </span>
          {fields.map(([target, spec]) => (
            <div key={`${target}:${epoch}`} className="map-field">
              <div className="value-row">
                <TargetPicker
                  label={`Target of ${target || 'new field'}`}
                  value={target}
                  options={targets}
                  onChange={(t) => {
                    if (t !== target && fields.some(([k]) => k === t)) return
                    setFields(fields.map(([k, v]) => (k === target ? [t, v] : [k, v])))
                  }}
                />
                <span className="spacer" />
                <Published target={target} schema={schema} />
                <Button
                  size="xs"
                  variant="ghost"
                  icon={<TbTrash />}
                  aria-label={`Remove ${target || 'field'}`}
                  title="Remove field"
                  onClick={() => {
                    setFields(fields.filter(([k]) => k !== target))
                    setEpoch(epoch + 1)
                  }}
                />
              </div>
              <ValueEditor label={target || 'new field'} value={spec as ValueSpec} onChange={(v) => setFields(fields.map(([k, x]) => (k === target ? [k, v] : [k, x])))} />
            </div>
          ))}
          <div>
            <Button size="sm" variant="secondary" icon={<TbPlus />} disabled={fields.some(([k]) => k === '')} onClick={() => setFields([...fields, ['', '']])}>
              Add field
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}
