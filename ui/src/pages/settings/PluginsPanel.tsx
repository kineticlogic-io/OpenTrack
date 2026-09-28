import { useCallback, useEffect, useState } from 'react'
import { TbPlayerPlay, TbPlus, TbShieldLock, TbTrash } from 'react-icons/tb'
import { Badge, Button, CollapsiblePanel, DataTable, FieldSelect, FileDropZone, Input, Label, Modal, Toggle, useToast, type DataTableColumn } from 'staresdk'
import { api, type PluginGrants, type PluginInfo } from '../../api/client'
import { InfoTip } from '../../components/InfoTip'
import { useCan } from '../../auth/context'
import { errorMessage } from '../../lib/format'
import { INPUT } from '../../lib/valueSpec'

const STATUS: Record<PluginInfo['status'], 'success' | 'danger' | 'grey' | 'warning'> = {
  loaded: 'success',
  error: 'danger',
  disabled: 'grey',
  loading: 'warning',
}

const DEFAULT_GRANTS: PluginGrants = { memory_mb: 256, call_timeout_ms: 5000, dirs: [], network: [], env: {} }

function size(bytes: number | null | undefined) {
  if (!bytes) return ''
  return bytes > 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.round(bytes / 1024)} KB`
}

function FormRow({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
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

/** Add a WebAssembly component, or an external plugin by its address. */
function AddPlugin({ onClose, onAdded }: { onClose: () => void; onAdded: (p: PluginInfo) => void }) {
  const { toast } = useToast()
  const [runtime, setRuntime] = useState<'wasm' | 'external'>('wasm')
  const [file, setFile] = useState<File | null>(null)
  const [address, setAddress] = useState('')
  const [replace, setReplace] = useState(false)
  const [busy, setBusy] = useState(false)
  const ready = runtime === 'wasm' ? file != null : address.trim() !== ''
  const add = async () => {
    setBusy(true)
    try {
      const p = runtime === 'wasm' && file ? await api.addPluginWasm(file, replace) : await api.addPluginExternal(address.trim(), replace)
      toast({ variant: 'success', title: 'Plugin added', message: `${p.name} ${p.version}: ${p.kinds.join(', ')}` })
      onAdded(p)
    } catch (e) {
      toast({ variant: 'error', title: 'Plugin not added', message: errorMessage(e) })
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal title="Add plugin" onClose={onClose} width={560}>
      <div className="panel-body stack">
        <FormRow
          label="Runs as"
          hint="WebAssembly: a component OpenTrack runs sandboxed, with only the memory, time, files and network you grant it. External: a program of its own (Python with numpy or Stone Soup, a GPU) serving the plugin interface on a socket."
        >
          <FieldSelect ariaLabel="Plugin runtime" fields={[{ name: 'wasm' }, { name: 'external' }]} value={runtime} onChange={(v) => setRuntime(v === 'external' ? 'external' : 'wasm')} style={{ width: 160 }} />
        </FormRow>
        {runtime === 'wasm' ? (
          <FileDropZone
            inputId="plugin-file"
            accept=".wasm,application/wasm"
            acceptedExtensions={['.wasm']}
            file={file}
            onFileChange={setFile}
            onReject={(m) => toast({ variant: 'error', title: 'Plugin', message: m })}
            label="A plugin component (.wasm)"
            hint="Built with the Rust or Python SDK (sdk/)."
            compact
          />
        ) : (
          <FormRow label="Address" hint="Where the plugin listens: host:port, tcp://host:port or unix:/path/to/socket.">
            <Input style={{ ...INPUT, width: 280 }} aria-label="Plugin address" placeholder="127.0.0.1:47300" value={address} onChange={(e) => setAddress(e.target.value)} spellCheck={false} />
          </FormRow>
        )}
        <FormRow label="Replace" hint="A plugin of the same name gets this build or address, and keeps its grants and whether it is enabled.">
          <Toggle size="sm" aria-label="Replace a plugin of the same name" value={replace} onChange={setReplace} />
        </FormRow>
        <div className="num-row" style={{ justifyContent: 'flex-end' }}>
          <Button size="sm" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button size="sm" icon={<TbPlus />} disabled={!ready || busy} onClick={add}>
            {busy ? 'Loading…' : 'Add'}
          </Button>
        </div>
      </div>
    </Modal>
  )
}

const lines = (s: string) =>
  s
    .split('\n')
    .map((x) => x.trim())
    .filter(Boolean)

/** What a WebAssembly plugin may use beyond computing. */
function GrantsEditor({ plugin, onClose, onSaved }: { plugin: PluginInfo; onClose: () => void; onSaved: (p: PluginInfo) => void }) {
  const { toast } = useToast()
  const g = { ...DEFAULT_GRANTS, ...(plugin.grants ?? {}) }
  const [memory, setMemory] = useState(String(g.memory_mb))
  const [timeout, setTimeoutMs] = useState(String(g.call_timeout_ms))
  const [network, setNetwork] = useState(g.network.join('\n'))
  const [dirs, setDirs] = useState(g.dirs.map((d) => [d.path, d.guest && d.guest !== d.path ? d.guest : '', d.write ? 'rw' : ''].filter((x, i) => x || i === 0).join(' ')).join('\n'))
  const [env, setEnv] = useState(Object.entries(g.env).map(([k, v]) => `${k}=${v}`).join('\n'))
  const [busy, setBusy] = useState(false)
  const external = plugin.runtime === 'external'
  const save = async () => {
    const grants: PluginGrants = {
      memory_mb: Number(memory),
      call_timeout_ms: Number(timeout),
      network: lines(network),
      dirs: lines(dirs).map((l) => {
        const [path, ...rest] = l.split(/\s+/)
        const write = rest.includes('rw')
        const guest = rest.find((x) => x !== 'rw')
        return { path, ...(guest ? { guest } : {}), ...(write ? { write } : {}) }
      }),
      env: Object.fromEntries(
        lines(env).map((l) => {
          const i = l.indexOf('=')
          return i < 0 ? [l, ''] : [l.slice(0, i), l.slice(i + 1)]
        }),
      ),
    }
    setBusy(true)
    try {
      onSaved(await api.configurePlugin(plugin.name, { grants }))
      toast({ variant: 'success', title: 'Grants saved', message: plugin.name })
    } catch (e) {
      toast({ variant: 'error', title: 'Grants not saved', message: errorMessage(e) })
    } finally {
      setBusy(false)
    }
  }
  const area = (label: string, value: string, set: (v: string) => void, placeholder: string) => (
    <textarea className="plain-textarea" aria-label={label} rows={3} value={value} onChange={(e) => set(e.target.value)} placeholder={placeholder} spellCheck={false} disabled={external} />
  )
  return (
    <Modal title={`Grants: ${plugin.name}`} onClose={onClose} width={600}>
      <div className="panel-body stack">
        {external && <p className="muted small">An external plugin runs where it was started, with what that gives it. Only its time per call applies here.</p>}
        <FormRow label="Memory (MB)" hint="Most memory the plugin may grow to.">
          <Input style={{ ...INPUT, width: 120 }} type="number" aria-label="Memory MB" value={memory} onChange={(e) => setMemory(e.target.value)} disabled={external} />
        </FormRow>
        <FormRow label="Time per call (ms)" hint="A call that runs longer is stopped, and the plugin instance is not used again.">
          <Input style={{ ...INPUT, width: 120 }} type="number" aria-label="Call timeout ms" value={timeout} onChange={(e) => setTimeoutMs(e.target.value)} />
        </FormRow>
        <FormRow label="Network" hint="Addresses it may connect to, one host:port a line (e.g. a model server). None: no network.">
          {area('Network', network, setNetwork, 'e.g. models.local:8500')}
        </FormRow>
        <FormRow label="Directories" hint="Server directories it can see, one a line: /absolute/path [where it sees it] [rw]. Read only unless rw.">
          {area('Directories', dirs, setDirs, 'e.g. /srv/opentrack/models')}
        </FormRow>
        <FormRow label="Environment" hint="Variables it sees, one KEY=value a line.">
          {area('Environment', env, setEnv, 'e.g. MODEL=radar-v2')}
        </FormRow>
        <div className="num-row" style={{ justifyContent: 'flex-end' }}>
          <Button size="sm" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button size="sm" disabled={busy} onClick={save}>
            Save
          </Button>
        </div>
      </div>
    </Modal>
  )
}

/** Settings → Plugins: the built-in and added plugins, what uses them, and their grants. */
export function PluginsPanel() {
  const { toast, confirm } = useToast()
  const [plugins, setPlugins] = useState<PluginInfo[] | null>(null)
  const [adding, setAdding] = useState(false)
  const [granting, setGranting] = useState<PluginInfo | null>(null)
  const admin = useCan('admin')
  const load = useCallback(() => {
    api.plugins().then(setPlugins, (e) => toast({ variant: 'error', title: 'Plugins', message: errorMessage(e) }))
  }, [toast])
  useEffect(load, [load])
  const replaceRow = (p: PluginInfo) => setPlugins((ps) => (ps ?? []).map((x) => (x.name === p.name ? p : x)))

  const toggle = async (p: PluginInfo, enabled: boolean) => {
    try {
      replaceRow(await api.configurePlugin(p.name, { enabled }))
    } catch (e) {
      toast({ variant: 'error', title: enabled ? 'Not enabled' : 'Not disabled', message: errorMessage(e) })
    }
  }
  const check = async (p: PluginInfo) => {
    try {
      const r = await api.checkPlugin(p.name)
      const kinds = Object.entries(r.kinds ?? {})
        .map(([k, v]) => (v === 'ok' ? `${k} ok` : `${k}: ${v.error}`))
        .join('; ')
      toast({ variant: r.ok ? 'success' : 'error', title: `${p.name} ${r.ok ? 'works' : 'failed'}`, message: r.error ?? `${kinds} (loaded in ${r.load_ms} ms)` })
    } catch (e) {
      toast({ variant: 'error', title: 'Check failed', message: errorMessage(e) })
    }
  }
  const remove = async (p: PluginInfo) => {
    const users = p.used_by.map((u) => (u.source ? `source ${u.name ?? u.source}` : 'correlation')).join(', ')
    const ok = await confirm(users ? `It is in use (${users}): those will fail until changed.` : 'Sources and settings can no longer use it.', {
      title: `Delete ${p.name}?`,
      confirmLabel: 'Delete',
    })
    if (!ok) return
    try {
      await api.deletePlugin(p.name, users !== '')
      setPlugins((ps) => (ps ?? []).filter((x) => x.name !== p.name))
    } catch (e) {
      toast({ variant: 'error', title: 'Not deleted', message: errorMessage(e) })
    }
  }

  const columns: DataTableColumn<PluginInfo>[] = [
    {
      key: 'name',
      header: 'Plugin',
      render: (p) => (
        <div className="stack" style={{ gap: 0 }}>
          <strong>{p.name}</strong>
          <span className="muted small" style={{ whiteSpace: 'normal' }}>
            {p.description}
          </span>
        </div>
      ),
      sortValue: (p) => p.name,
    },
    { key: 'version', header: 'Version', render: (p) => p.version, width: 90, mono: true },
    {
      key: 'kinds',
      header: 'Provides',
      render: (p) => (
        <span className="num-row">
          {p.kinds.map((k) => (
            <Badge key={k} size="sm" color="blue">
              {k}
            </Badge>
          ))}
        </span>
      ),
      width: 150,
    },
    {
      key: 'runtime',
      header: 'Runs as',
      render: (p) =>
        p.runtime === 'wasm' ? (
          <div className="stack" style={{ gap: 0 }}>
            <span>WebAssembly {size(p.size)}</span>
            {p.sha256 && (
              <span className="mono muted small" title={p.sha256}>
                sha256 {p.sha256.slice(0, 12)}…
              </span>
            )}
          </div>
        ) : p.runtime === 'external' ? (
          <span className="mono">{p.address}</span>
        ) : (
          'built in'
        ),
      width: 170,
    },
    {
      key: 'status',
      header: 'Status',
      render: (p) => (
        <div className="stack" style={{ gap: 0 }}>
          <Badge size="sm" color={STATUS[p.status]}>
            {p.status}
          </Badge>
          {p.error && (
            <span className="error-text small" style={{ whiteSpace: 'normal' }}>
              {p.error}
            </span>
          )}
        </div>
      ),
      width: 90,
    },
    {
      key: 'used',
      header: 'Used by',
      render: (p) => (p.used_by.length ? p.used_by.map((u) => (u.source ? `${u.name ?? u.source} (${u.as})` : 'correlation')).join(', ') : <span className="muted">—</span>),
    },
    {
      key: 'actions',
      header: '',
      align: 'right',
      width: 190,
      render: (p) =>
        p.builtin ? null : (
          <span className="num-row" style={{ justifyContent: 'flex-end' }}>
            <Toggle size="sm" aria-label={`Enable ${p.name}`} value={p.enabled} disabled={!admin} onChange={(v) => toggle(p, v)} />
            <Button size="xs" variant="ghost" icon={<TbPlayerPlay />} title="Check it loads and opens" aria-label={`Check ${p.name}`} disabled={!admin} onClick={() => check(p)} />
            <Button size="xs" variant="ghost" icon={<TbShieldLock />} title="Grants" aria-label={`Grants for ${p.name}`} disabled={!admin} onClick={() => setGranting(p)} />
            <Button size="xs" variant="ghost" icon={<TbTrash />} title="Delete" aria-label={`Delete ${p.name}`} disabled={!admin} onClick={() => remove(p)} />
          </span>
        ),
    },
  ]

  return (
    <CollapsiblePanel
      title="Plugins"
      persistKey="ot.panel.settings.plugins"
      titleActions={
        <InfoTip label="Plugins">
          Codecs, trackers and pairing scorers of your own, beside the built-in ones. A WebAssembly plugin runs sandboxed inside OpenTrack with only what
          you grant it; an external one is a program of its own that OpenTrack connects to. Sources pick codec and tracker plugins in their pipeline;
          correlation settings pick a scorer. Write them with the SDKs in sdk/ (Rust, Python).
          <br />
          <br />
          Provides: what it can be used as (codec, tracker, scorer). Runs as: built in, WebAssembly (with its size and the start of its SHA-256
          fingerprint) or the address of an external plugin. Status: loaded, loading, disabled, or error with the reason. The switch enables
          it: off unloads it, so a source using it cannot start until it is back on, and a correlation scorer falls back to the kinematic score.
          ▶ checks it loads and opens; the shield edits its grants.
        </InfoTip>
      }
      actions={
        admin && (
          <Button size="sm" icon={<TbPlus />} onClick={() => setAdding(true)}>
            Add plugin
          </Button>
        )
      }
    >
      <div className="panel-body">
        {plugins === null ? (
          <span className="muted">LOADING…</span>
        ) : (
          <DataTable aria-label="Plugins" columns={columns} rows={plugins} rowKey={(p) => p.name} density="compact" empty="No plugins." />
        )}
      </div>
      {adding && (
        <AddPlugin
          onClose={() => setAdding(false)}
          onAdded={() => {
            setAdding(false)
            load()
          }}
        />
      )}
      {granting && (
        <GrantsEditor
          plugin={granting}
          onClose={() => setGranting(null)}
          onSaved={(p) => {
            replaceRow(p)
            setGranting(null)
          }}
        />
      )}
    </CollapsiblePanel>
  )
}
