import { useEffect, useState } from 'react'
import { TbDownload, TbTrash } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, Input, Label, SaveButton, Toggle, useToast } from 'staresdk'
import { api, type AppSettings, type AppSettingsResponse } from '../../api/client'
import { InfoTip } from '../../components/InfoTip'
import { NodesPanel } from './NodesPanel'
import { PluginsPanel } from './PluginsPanel'
import { SecurityPanel } from './SecurityPanel'
import { UsersPanel } from './UsersPanel'
import { useCan } from '../../auth/context'
import { errorMessage } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'

/** OpenStare's classification presets (background, text colour). */
const PRESETS = [
  { label: 'Unclassified', background: '#006400', color: '#ffffff' },
  { label: 'CUI', background: '#502b85', color: '#ffffff' },
  { label: 'Confidential', background: '#0033a0', color: '#ffffff' },
  { label: 'Secret', background: '#c8102e', color: '#ffffff' },
  { label: 'Top Secret', background: '#ff8300', color: '#000000' },
]

const SITE_CODE_INFO =
  'Site code: the 3 characters (A–Z, 0–9) that begin every track number this instance issues, e.g. OTK000000042 — ' +
  "OTH-GOLD's track UID form, a site code then a 9-digit sequence. It keeps track numbers unique between the sites feeding one " +
  'picture, so every OpenTrack needs its own. It is set at deployment (OT_SITE_CODE) and cannot change here: published track numbers would change with it.'

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="settings-row">
      <div className="row-label">
        <Label size="sm">{label}</Label>
        {hint && <InfoTip label={label}>{hint}</InfoTip>}
      </div>
      <div className="settings-row-control">{children}</div>
    </div>
  )
}

/** Instance settings: site name, banners, plugins, users and security (admins), data export, purge. */
export default function SettingsPage({ onSaved }: { onSaved: () => void }) {
  const { toast, confirm } = useToast()
  const [loaded, setLoaded] = useState<AppSettingsResponse | null>(null)
  const [draft, setDraft] = useState<AppSettings | null>(null)
  const [purgeConfirm, setPurgeConfirm] = useState('')
  const [purgeHistory, setPurgeHistory] = useState(false)
  const [purging, setPurging] = useState(false)
  const [saving, setSaving] = useState(false)
  const [saved, setSaved] = useState(false)
  const admin = useCan('admin')

  useEffect(() => {
    api.appSettings().then(
      (r) => {
        setLoaded(r)
        setDraft(r.settings)
      },
      (e) => toast({ variant: 'error', title: 'Settings', message: errorMessage(e) }),
    )
  }, [toast])

  if (!draft || !loaded) return <span className="muted">LOADING…</span>
  const b = draft.banner
  const setB = (patch: Partial<AppSettings['banner']>) => setDraft({ ...draft, banner: { ...b, ...patch } })
  const w = draft.warning ?? { enabled: false, text: '' }
  const setW = (patch: Partial<AppSettings['warning']>) => setDraft({ ...draft, warning: { ...w, ...patch } })
  const dirty = JSON.stringify(draft) !== JSON.stringify(loaded.settings)
  const hours = draft.history_hours ?? 12
  const interval = draft.history_interval_secs ?? 10
  const perThousand = interval > 0 ? (1000 * hours * 3600 * 130) / interval : null
  const historyEstimate =
    perThousand == null || hours <= 0 ? null : perThousand >= 1e9 ? `${(perThousand / 1e9).toFixed(1)} GB` : `${Math.round(perThousand / 1e6)} MB`

  const save = async () => {
    setSaving(true)
    setSaved(false)
    try {
      const r = await api.saveAppSettings(draft)
      setLoaded(r)
      setDraft(r.settings)
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

  return (
    <div className="stack">
      <CollapsiblePanel title="Instance" persistKey="ot.panel.settings.instance" actions={admin && <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}>
        <div className="panel-body">
          <Row label="Site name" hint="A name for this instance, up to 64 characters, shown in the header and browser tab as OpenTrack · name. Display only: track numbers use the site code.">
            <div className="num-row">
              <Input style={{ ...INPUT, width: 260 }} aria-label="Site name" value={draft.site_name} maxLength={64} onChange={(e) => setDraft({ ...draft, site_name: e.target.value })} />
              <span className="muted">site code</span>
              <Badge color="grey" size="sm">
                {loaded.site_code}
              </Badge>
              <InfoTip label="Site code">{SITE_CODE_INFO}</InfoTip>
            </div>
          </Row>
          <Row label="Position history">
            <div className="num-row">
              <Input
                style={{ ...INPUT, width: 90 }}
                type="number"
                min={0}
                max={720}
                step="any"
                aria-label="History hours"
                placeholder="12"
                value={draft.history_hours ?? ''}
                onChange={(e) => setDraft({ ...draft, history_hours: e.target.value === '' ? null : Number(e.target.value) })}
              />
              <span className="muted">hours, a point every</span>
              <Input
                style={{ ...INPUT, width: 90 }}
                type="number"
                min={0}
                max={3600}
                step="any"
                aria-label="History interval seconds"
                placeholder="10"
                value={draft.history_interval_secs ?? ''}
                onChange={(e) => setDraft({ ...draft, history_interval_secs: e.target.value === '' ? null : Number(e.target.value) })}
              />
              <span className="muted">s</span>
              <InfoTip label="Position history">
                How long each track&apos;s positions are kept (0: none, at most 720; empty: 12 h), and at most one point per track this often (0: every
                update, at most 3600; empty: 10 s). Memory in Redis is about 130 bytes a point; points per track = hours × 3600 ÷ interval. For example
                2,000 tracks for 12 h every 10 s take about 1.1 GB; 16,000 tracks about 9 GB.
              </InfoTip>
              {historyEstimate && <span className="muted">≈ {historyEstimate} per 1,000 tracks</span>}
            </div>
          </Row>
        </div>
      </CollapsiblePanel>

      {/* As OpenStare's Banner settings: this instance's own marking, whatever OpenStare shows. */}
      <CollapsiblePanel title="Banners" persistKey="ot.panel.settings.banner" actions={admin && <SaveButton size="sm" dirty={dirty} saving={saving} saved={saved} onSave={save} />}>
        <div className="panel-body banner-settings">
          <div className="banner-toggle">
            <div className="field-caps">
              Classification banner
              <InfoTip label="Classification banner">Displays a fixed bar at the top and bottom of every page.</InfoTip>
            </div>
            <Toggle value={b.enabled} onChange={(enabled) => setB({ enabled })} aria-label="Classification Banner" />
          </div>
          <div>
            <div className="field-caps">
              Classification text
              <InfoTip label="Classification text">The marking shown in the banner, 1 to 128 characters (required while the banner is on).</InfoTip>
            </div>
            <Input style={{ width: '100%' }} value={b.text} maxLength={128} onChange={(e) => setB({ text: e.target.value })} placeholder="e.g. UNCLASSIFIED // FOR OFFICIAL USE ONLY" />
          </div>
          <div>
            <div className="field-caps">
              Color preset
              <InfoTip label="Color preset">Sets the background and text colours to the usual ones for that marking. Adjust them below if needed.</InfoTip>
            </div>
            <div className="banner-presets">
              {PRESETS.map((p) => (
                <button
                  key={p.label}
                  type="button"
                  className={b.background === p.background ? 'banner-preset selected' : 'banner-preset'}
                  style={{ background: p.background, color: p.color }}
                  onClick={() => setB({ background: p.background, color: p.color })}
                >
                  {p.label}
                </button>
              ))}
            </div>
          </div>
          <div className="banner-colours">
            {(
              [
                ['background', 'Background color', '#006400'],
                ['color', 'Text color', '#ffffff'],
              ] as const
            ).map(([key, label, placeholder]) => (
              <div key={key}>
                <div className="field-caps">
                  {label}
                  <InfoTip label={label}>Pick a colour or type it as #rrggbb.</InfoTip>
                </div>
                <div className="banner-colour">
                  <input type="color" aria-label={label} value={b[key]} onChange={(e) => setB({ [key]: e.target.value })} />
                  <Input style={{ flex: 1 }} aria-label={`${label} hex`} value={b[key]} onChange={(e) => setB({ [key]: e.target.value })} placeholder={placeholder} />
                </div>
              </div>
            ))}
          </div>
          <div>
            <div className="field-caps">Preview</div>
            <div className="banner-preview" style={{ background: b.background, color: b.color, opacity: b.enabled ? 1 : 0.35 }}>
              {b.text || 'UNCLASSIFIED'}
            </div>
          </div>
          <div className="banner-toggle">
            <div className="field-caps">
              Warning banner
              <InfoTip label="Warning banner">
                A notice users must accept after signing in, such as consent to monitoring. Declining signs them out. Asked once per browser session.
              </InfoTip>
            </div>
            <Toggle value={w.enabled} onChange={(enabled) => setW({ enabled })} aria-label="Warning Banner" />
          </div>
          <div>
            <div className="field-caps">
              Warning text
              <InfoTip label="Warning text">What users read and accept or decline, up to 20,000 characters. Line breaks are kept.</InfoTip>
            </div>
            <textarea
              className="plain-textarea"
              style={{ maxWidth: 'none', fontFamily: 'var(--font-sans)' }}
              aria-label="Warning text"
              rows={8}
              maxLength={20000}
              value={w.text}
              onChange={(e) => setW({ text: e.target.value })}
              placeholder="e.g. You are accessing a U.S. Government information system…"
            />
          </div>
        </div>
      </CollapsiblePanel>

      {admin && <UsersPanel />}
      {admin && <SecurityPanel />}
      {admin && (
        <NodesPanel
          value={draft.sync}
          onChange={(sync) => setDraft({ ...draft, sync })}
          siteCode={loaded.site_code}
          dirty={dirty}
          saving={saving}
          saved={saved}
          onSave={save}
        />
      )}

      <PluginsPanel />

      <CollapsiblePanel title="Data export" persistKey="ot.panel.settings.export">
        <div className="panel-body">
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
          <Row label="Configuration" hint="Sources, output schema versions, correlation and instance settings, as one JSON file for backup. Admins only.">
            <Button size="sm" variant="ghost" disabled={!admin} icon={<TbDownload />} onClick={() => window.open(api.exportUrl('config'), '_self')}>
              Configuration
            </Button>
          </Row>
          <Row label="Registry" hint="Entities with their identifiers and attributes: export and import them on the Registry tab.">
            <Button size="sm" variant="ghost" icon={<TbDownload />} onClick={() => window.open(api.registryExportUrl('xlsx'), '_self')}>
              Registry XLSX
            </Button>
          </Row>
        </div>
      </CollapsiblePanel>

      <CollapsiblePanel
        title="Purge"
        persistKey="ot.panel.settings.purge"
        titleActions={
          <InfoTip label="Purge">
            Retire every live track, and delete the published ones in OpenStare. Sources, the output schema, the registry and the decision log stay.
          </InfoTip>
        }
      >
        <div className="panel-body">
          <Row label="Also delete history" hint="The track graph: how every track was formed, paired, merged and split.">
            <Toggle size="sm" aria-label="Also delete history" value={purgeHistory} onChange={setPurgeHistory} />
          </Row>
          <Row label="Confirm" hint={`Type the site code, ${loaded.site_code}.`}>
            <div className="num-row">
              <Input style={{ ...INPUT, width: 120 }} aria-label="Site code to confirm" value={purgeConfirm} onChange={(e) => setPurgeConfirm(e.target.value)} spellCheck={false} />
              <Button size="sm" variant="danger" icon={<TbTrash />} disabled={!admin || purging || purgeConfirm.trim() !== loaded.site_code} onClick={purge}>
                Purge tracks
              </Button>
            </div>
          </Row>
        </div>
      </CollapsiblePanel>
    </div>
  )
}
