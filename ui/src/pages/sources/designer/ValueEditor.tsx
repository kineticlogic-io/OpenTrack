import { useState } from 'react'
import { TbTable, TbX } from 'react-icons/tb'
import { Button, FieldSelect, Input } from 'staresdk'
import type { ValueSpec } from '../../../lib/pipeline'
import { fromForm, INPUT, toForm, type Mode, type ValueForm } from '../../../lib/valueSpec'
import { JsonField } from './JsonField'

const MODES: { mode: Mode; name: string }[] = [
  { mode: 'path', name: 'feed field' },
  { mode: 'first', name: 'first present of' },
  { mode: 'const', name: 'fixed value' },
  { mode: 'template', name: 'template' },
  { mode: 'advanced', name: 'advanced (JSON)' },
]

/**
 * Where a value comes from and what is done to it, as a form: a feed field (or the first present
 * of several, a fixed value, a template), then transforms, values to ignore, a lookup table and a
 * default. Richer expressions are edited as JSON. Remount (change `key`) to load another value.
 */
export function ValueEditor({ label, value, onChange }: { label: string; value: ValueSpec | undefined; onChange: (v: ValueSpec) => void }) {
  const [form, setForm] = useState<ValueForm>(() => toForm(value))
  const [json, setJson] = useState<unknown>(value)

  const update = (patch: Partial<ValueForm>) => {
    const next = { ...form, ...patch }
    setForm(next)
    if (next.mode !== 'advanced') onChange(fromForm(next))
  }
  const switchMode = (name: string | null) => {
    const mode = MODES.find((m) => m.name === name)?.mode ?? 'path'
    if (mode === 'advanced') {
      const spec = form.mode === 'advanced' ? json : fromForm(form)
      setJson(spec)
      setForm({ ...form, mode })
      return
    }
    // Carry the source across: a path becomes the first of a list, and back.
    const seed = form.path || form.first.split(',')[0]?.trim() || ''
    update({ mode, path: form.path || seed, first: form.first || seed })
  }

  return (
    <div className="value-editor">
      <div className="value-row">
        <FieldSelect
          ariaLabel={`${label}: source`}
          fields={MODES.map((m) => ({ name: m.name }))}
          value={MODES.find((m) => m.mode === form.mode)!.name}
          onChange={switchMode}
          style={{ width: 150 }}
        />
        {form.mode === 'path' && (
          <Input style={INPUT} aria-label={`${label}: feed field`} placeholder="feed field" value={form.path} onChange={(e) => update({ path: e.target.value })} spellCheck={false} />
        )}
        {form.mode === 'first' && (
          <Input style={INPUT} aria-label={`${label}: fields to try`} placeholder="flight, r, hex" value={form.first} onChange={(e) => update({ first: e.target.value })} spellCheck={false} />
        )}
        {form.mode === 'const' && (
          <Input style={INPUT} aria-label={`${label}: value`} placeholder="value" value={form.constant} onChange={(e) => update({ constant: e.target.value })} spellCheck={false} />
        )}
        {form.mode === 'template' && (
          <Input style={INPUT} aria-label={`${label}: template`} placeholder="a-{aff}-A" value={form.template} onChange={(e) => update({ template: e.target.value })} spellCheck={false} />
        )}
      </div>
      {form.mode === 'advanced' ? (
        <JsonField
          label={`${label}: JSON`}
          value={json}
          onChange={(v) => {
            setJson(v)
            if (v !== undefined) onChange(v as ValueSpec)
          }}
        />
      ) : (
        form.mode !== 'const' && (
          <>
            <div className="value-row three">
              <Input
                style={INPUT}
                aria-label={`${label}: transforms`}
                placeholder="transforms"
                value={form.transforms}
                onChange={(e) => update({ transforms: e.target.value })}
                spellCheck={false}
              />
              <Input style={INPUT} aria-label={`${label}: ignore`} placeholder="ignore values" value={form.nullIf} onChange={(e) => update({ nullIf: e.target.value })} spellCheck={false} />
              <Input style={INPUT} aria-label={`${label}: default`} placeholder="default" value={form.defaultValue} onChange={(e) => update({ defaultValue: e.target.value })} spellCheck={false} />
            </div>
            {form.table ? (
              <div className="stack" style={{ gap: 4 }}>
                <div className="value-row">
                  <span className="muted">Lookup table · {Object.keys(form.table).length}</span>
                  <span className="spacer" />
                  <Input style={{ ...INPUT, width: 180 }} aria-label={`${label}: table default`} placeholder="otherwise" value={form.tableDefault} onChange={(e) => update({ tableDefault: e.target.value })} spellCheck={false} />
                  <Button size="xs" variant="ghost" icon={<TbX />} aria-label="Remove lookup table" title="Remove lookup table" onClick={() => update({ table: null, tableDefault: '' })} />
                </div>
                <JsonField
                  label={`${label}: lookup table`}
                  value={form.table}
                  maxHeight={160}
                  onChange={(v) => v && typeof v === 'object' && !Array.isArray(v) && update({ table: v as Record<string, unknown> })}
                />
              </div>
            ) : (
              <div>
                <Button size="sm" variant="secondary" icon={<TbTable />} onClick={() => update({ table: {} })}>
                  Lookup table
                </Button>
              </div>
            )}
          </>
        )
      )}
    </div>
  )
}

