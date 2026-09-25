import { useEffect, useState } from 'react'
import { TbDownload, TbTrash } from 'react-icons/tb'
import { Button, CollapsiblePanel, FieldSelect, Input, Label, SaveButton, Toggle, useToast } from 'staresdk'
import { api, type AppSettings, type AppSettingsResponse, type Banner } from '../../api/client'
import { errorMessage } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'

/** Standard US classification markings (text, background, text colour). */
const PRESETS: { label: string; text: string; background: string; color: string }[] = [
  { label: 'UNCLASSIFIED', text: 'UNCLASSIFIED', background: '#007a33', color: '#ffffff' },
  { label: 'CUI', text: 'CUI', background: '#502b85', color: '#ffffff' },
  { label: 'CONFIDENTIAL', text: 'CONFIDENTIAL', background: '#0033a0', color: '#ffffff' },
  { label: 'SECRET', text: 'SECRET', background: '#c8102e', color: '#ffffff' },
  { label: 'TOP SECRET', text: 'TOP SECRET', background: '#ff8c00', color: '#000000' },
  { label: 'TOP SECRET//SCI', text: 'TOP SECRET//SCI', background: '#fce83a', color: '#000000' },
]

const MODES = [
  { name: 'off', label: 'Off' },
  { name: 'manual', label: 'Set here' },
  { name: 'openstare', label: 'Follow OpenStare' },
]

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
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

/** Instance settings: site name, classification banner, data export, purge. */
export default function SettingsPage({ onSaved }: { onSaved: () => void }) {
  const { toast, confirm } = useToast()
  const [loaded, setLoaded] = useState<AppSettingsResponse | null>(null)
  const [draft, setDraft] = useState<AppSettings | null>(null)
  const [effective, setEffective] = useState<{ source: string; error?: string; banner: Banner } | null>(null)
  const [purgeConfirm, setPurgeConfirm] = useState('')
  const [purgeHistory, setPurgeHistory] = useState(false)
  const [purging, setPurging] = useState(false)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    api.appSettings().then(
      (r) => {
        setLoaded(r)
        setDraft(r.settings)
      },
      (e) => toast({ variant: 'error', title: 'Settings', message: errorMessage(e) }),
    )
    api.banner().then(setEffective, () => setEffective(null))
  }, [toast])

  if (!draft || !loaded) return <span className="muted">LOADING…</span>
  const b = draft.banner
  const setB = (patch: Partial<AppSettings['banner']>) => setDraft({ ...draft, banner: { ...b, ...patch } })
  const dirty = JSON.stringify(draft) !== JSON.stringify(loaded.settings)

  const save = async () => {
    setSaving(true)
    setSaved(false)
    try {
      const r = await api.saveAppSettings(draft)
      setLoaded(r)
      setDraft(r.settings)
      setEffective(await api.banner())
      onSaved()
      setSaved(true)
    } catch (e) {
      toast({ variant: 'error', title: 'Not saved', message: errorMessage(e) })
    } finally {
      setSaving(false)
    }
  }

  const purge = async () => {
    const ok = await confirm(
      `Every live track is retired now, and the published ones are deleted in OpenStare too${purgeHistory ? '; how each track was formed and paired is deleted as well' : ''}. Sources keep reporting, so new tracks will appear. This cannot be undone.`,
      { title: 'Purge every track', confirmLabel: 'Purge' },
    )
    if (!ok) return
    setPurging(true)
    try {
      const r = await api.purge(purgeConfirm, purgeHistory)
      toast({ variant: 'success', title: 'Purged', message: `${r.retired} tracks retired${r.history ? `, ${r.history.nodes} graph nodes deleted` : ''}` })
      setPurgeConfirm('')
    } catch (e) {
      toast({ variant: 'error', title: 'Not purged', message: errorMessage(e) })
    } finally {
      setPurging(false)
    }
  }

  const preview = b.mode === 'manual' ? { enabled: true, text: b.text, background: b.background, color: b.color } : effective?.banner
  return (
    <div className="stack">
      <CollapsiblePanel title="Instance" persistKey="ot.panel.settings.instance" actions={<SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}>
        <Row label="Site name" hint={`Shown in the header. Track UIDs keep the site code ${loaded.site_code} (set at deployment).`}>
          <Input style={{ ...INPUT, width: 260 }} aria-label="Site name" value={draft.site_name} maxLength={64} onChange={(e) => setDraft({ ...draft, site_name: e.target.value })} />
        </Row>
        <h4 className="subhead">Classification banner</h4>
        <Row label="Banner" hint="Top and bottom of every page, as in OpenStare. Following OpenStare reads its banner every minute; if OpenStare cannot be reached, the banner set here stays up.">
          <FieldSelect
            ariaLabel="Banner mode"
            fields={MODES.map((m) => ({ name: m.label }))}
            value={MODES.find((m) => m.name === b.mode)?.label ?? 'Off'}
            onChange={(label) => setB({ mode: (MODES.find((m) => m.label === label)?.name ?? 'off') as AppSettings['banner']['mode'] })}
            style={{ width: 200 }}
          />
        </Row>
        {b.mode === 'openstare' && (
          <Row label="OpenStare URL">
            <Input style={{ ...INPUT, width: 320 }} aria-label="OpenStare URL" value={b.openstare_url} onChange={(e) => setB({ openstare_url: e.target.value })} spellCheck={false} />
          </Row>
        )}
        {b.mode !== 'off' && (
          <>
            <Row label={b.mode === 'openstare' ? 'Fallback marking' : 'Marking'}>
              <div className="num-row">
                {PRESETS.map((p) => (
                  <Button key={p.label} size="xs" variant="ghost" onClick={() => setB({ text: p.text, background: p.background, color: p.color })}>
                    {p.label}
                  </Button>
                ))}
              </div>
            </Row>
            <Row label="Text">
              <Input style={{ ...INPUT, width: 320 }} aria-label="Banner text" value={b.text} maxLength={128} onChange={(e) => setB({ text: e.target.value })} />
            </Row>
            <Row label="Colours">
              <div className="num-row">
                <input type="color" aria-label="Banner background" value={b.background} onChange={(e) => setB({ background: e.target.value })} />
                <span className="muted">background</span>
                <input type="color" aria-label="Banner text colour" value={b.color} onChange={(e) => setB({ color: e.target.value })} />
                <span className="muted">text</span>
              </div>
            </Row>
          </>
        )}
        {preview?.enabled && (
          <Row label="Showing" hint={effective?.error ? `OpenStare could not be read (${effective.error}); showing the marking set here.` : undefined}>
            <div className="classification-preview" style={{ background: preview.background, color: preview.color }}>
              {preview.text}
            </div>
          </Row>
        )}
      </CollapsiblePanel>

      <CollapsiblePanel title="Data export" persistKey="ot.panel.settings.export">
        <Row label="Live tracks" hint="Every live track as published: the GOLD fields, attributes, state, confidence and sources.">
          <div className="num-row">
            <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.exportUrl('tracks.geojson'), '_self')}>
              GeoJSON
            </Button>
            <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.exportUrl('tracks.csv'), '_self')}>
              CSV
            </Button>
          </div>
        </Row>
        <Row label="Configuration" hint="Sources, output schema versions, correlation and instance settings, as one JSON file for backup.">
          <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.exportUrl('config'), '_self')}>
            Configuration
          </Button>
        </Row>
        <Row label="Registry" hint="Entities, identifiers and cards: export and import them on the Registry tab.">
          <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.registryExportUrl('xlsx'), '_self')}>
            Registry XLSX
          </Button>
        </Row>
      </CollapsiblePanel>

      <CollapsiblePanel title="Purge" persistKey="ot.panel.settings.purge">
        <span className="muted">
          Retire every live track, and delete the published ones in OpenStare. Sources, the output schema, the registry, cards and the decision log stay.
        </span>
        <Row label="Also delete history" hint="The track graph: how every track was formed, paired, merged and split.">
          <Toggle size="sm" aria-label="Also delete history" value={purgeHistory} onChange={setPurgeHistory} />
        </Row>
        <Row label="Confirm" hint={`Type the site code, ${loaded.site_code}.`}>
          <div className="num-row">
            <Input style={{ ...INPUT, width: 120 }} aria-label="Site code to confirm" value={purgeConfirm} onChange={(e) => setPurgeConfirm(e.target.value)} spellCheck={false} />
            <Button size="sm" variant="danger" icon={<TbTrash />} disabled={purging || purgeConfirm.trim() !== loaded.site_code} onClick={purge}>
              Purge tracks
            </Button>
          </div>
        </Row>
      </CollapsiblePanel>
    </div>
  )
}
