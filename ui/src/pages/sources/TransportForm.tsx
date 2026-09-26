import { useEffect, useState } from 'react'
import { CollapsiblePanel, FieldSelect, Input, Label, Toggle } from 'staresdk'
import { CodeEditor } from 'staresdk/code-editor'
import { api, type PluginInfo, type SourceSpec } from '../../api/client'
import { InfoTip } from '../../components/InfoTip'
import { PluginOptions, PluginPicker } from '../../components/PluginOptions'

type Transport = SourceSpec['transport']
type Codec = SourceSpec['pipeline']['codec']

const TRANSPORTS: Record<string, { label: string; initial: Transport }> = {
  http_poll: { label: 'HTTP poll', initial: { type: 'http_poll', url: '', interval_secs: 5 } },
  websocket: { label: 'WebSocket client', initial: { type: 'websocket', url: '' } },
  tcp_client: { label: 'TCP client', initial: { type: 'tcp_client', host: '', port: 0, framing: { type: 'lines' } } },
  tcp_server: { label: 'TCP server (listen)', initial: { type: 'tcp_server', bind: '0.0.0.0:8087', framing: { type: 'lines' } } },
  udp: { label: 'UDP (unicast or multicast)', initial: { type: 'udp', bind: '0.0.0.0:6969' } },
  mqtt: { label: 'MQTT subscribe', initial: { type: 'mqtt', url: '', topics: [] } },
}

const QOS: Record<string, { label: string }> = {
  '0': { label: '0 · at most once' },
  '1': { label: '1 · at least once' },
}

const FRAMINGS: Record<string, { label: string; initial: Record<string, unknown> }> = {
  lines: { label: 'Newline', initial: { type: 'lines' } },
  end_tag: { label: 'End tag (e.g. </event>)', initial: { type: 'end_tag', tag: '</event>' } },
  delimiter: { label: 'Delimiter', initial: { type: 'delimiter', delimiter: '\\n' } },
  length_prefix: { label: 'Length prefix', initial: { type: 'length_prefix', width: 'u32', endian: 'big' } },
  length_field: { label: 'Length field in header', initial: { type: 'length_field', offset: 0, width: 'u32', endian: 'big' } },
}

const CODECS: Record<string, { label: string; initial: Codec }> = {
  json: { label: 'JSON', initial: { type: 'json' } },
  cot_xml: { label: 'Cursor-on-Target XML', initial: { type: 'cot_xml' } },
  xml: { label: 'XML (generic)', initial: { type: 'xml', record_element: 'record' } },
  plugin: { label: 'Plugin', initial: { type: 'plugin', plugin: '', options: {} } },
}

/** FieldSelect works on names; map labels ↔ ids. */
function Select({
  options,
  value,
  onChange,
  ariaLabel,
}: {
  options: Record<string, { label: string }>
  value: string
  onChange: (id: string) => void
  ariaLabel: string
}) {
  const byLabel = Object.fromEntries(Object.entries(options).map(([id, o]) => [o.label, id]))
  return (
    <FieldSelect
      ariaLabel={ariaLabel}
      fields={Object.values(options).map((o) => ({ name: o.label }))}
      value={options[value]?.label ?? null}
      onChange={(label) => label && onChange(byLabel[label])}
    />
  )
}

function Text({
  label,
  value,
  onChange,
  placeholder,
  wide,
  type = 'text',
  help,
}: {
  label: string
  value: unknown
  onChange: (v: string) => void
  placeholder?: string
  wide?: boolean
  type?: string
  help?: string
}) {
  const id = `f-${label.replace(/\W+/g, '-').toLowerCase()}`
  return (
    <div className={wide ? 'field wide' : 'field'} title={help}>
      <Label htmlFor={id} size="sm">
        {label}
      </Label>
      <Input
        id={id}
        type={type}
        value={value === undefined || value === null ? '' : String(value)}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        spellCheck={false}
        autoComplete="off"
      />
    </div>
  )
}

/** A JSON-valued setting (headers, subscribe message) edited in a small code editor. */
function JsonSetting({ label, value, onChange }: { label: string; value: unknown; onChange: (v: unknown) => void }) {
  const [text, setText] = useState(() => (value === undefined ? '' : JSON.stringify(value, null, 2)))
  return (
    <div className="field wide">
      <Label size="sm">{label}</Label>
      <CodeEditor
        aria-label={label}
        value={text}
        minHeight={60}
        maxHeight={180}
        placeholder="JSON, optional. Use ${env:NAME} for secrets."
        onChange={(t) => {
          setText(t)
          if (t.trim() === '') return onChange(undefined)
          try {
            onChange(JSON.parse(t))
          } catch {
            // Keep the last valid value; the editor marks the parse error.
          }
        }}
      />
    </div>
  )
}

/** A list edited as comma-separated text (e.g. MQTT topic filters). */
function ListSetting({
  label,
  value,
  onChange,
  placeholder,
}: {
  label: string
  value: unknown
  onChange: (v: string[]) => void
  placeholder?: string
}) {
  const [text, setText] = useState(() => (Array.isArray(value) ? value.join(', ') : ''))
  return (
    <Text
      label={label}
      wide
      value={text}
      placeholder={placeholder}
      onChange={(t) => {
        setText(t)
        onChange(
          t
            .split(',')
            .map((s) => s.trim())
            .filter(Boolean),
        )
      }}
    />
  )
}

const num = (s: string) => (s.trim() === '' ? undefined : Number(s))

/** Transports that can run over TLS (the URL ones only for https://, wss://, mqtts://). */
const TLS_TRANSPORTS = ['tcp_client', 'tcp_server', 'http_poll', 'websocket', 'mqtt']

/** The transport's `tls` settings: trust, client certificate (mutual TLS) and, for the TCP server, its own certificate. */
function TlsSettings({ transport, onTransport }: { transport: Transport; onTransport: (t: Transport) => void }) {
  const server = transport.type === 'tcp_server'
  const tls = (transport.tls as Record<string, unknown> | undefined) ?? {}
  const setTls = (k: string, v: unknown) => {
    const nextTls = { ...tls, [k]: v }
    if (v === undefined || v === '' || v === false) delete nextTls[k]
    const next: Transport = { ...transport, tls: nextTls }
    // MQTT's older top-level ca_file moves into tls on edit.
    if (k === 'ca_file') delete next.ca_file
    if (Object.keys(nextTls).length === 0) delete next.tls
    onTransport(next)
  }
  const on = transport.tls !== undefined || transport.ca_file !== undefined
  return (
    <div className="field wide">
      <CollapsiblePanel
        title="TLS"
        badge={on ? (tls.cert_file || tls.client_ca_file ? 'mutual' : 'on') : undefined}
        defaultOpen={on}
        titleActions={
          server ? (
            <InfoTip label="TLS">
              Set a certificate and key (PEM files) to accept TLS connections only. With a client CA, every client must present a
              certificate that CA signed (mutual TLS); others are rejected, and each client&apos;s certificate subject is logged. Paths
              may use {'${env:NAME}'}.
            </InfoTip>
          ) : (
            <InfoTip label="TLS">
              For tcp, https://, wss:// and mqtts://. The CA file (PEM) is trusted instead of the system roots. A client certificate
              and key (PEM) authenticate this server to the feed (mutual TLS); set both or neither. Server name verifies the
              feed&apos;s certificate against that name instead of the host. Skipping verification lets anyone impersonate the feed:
              development only. Paths may use {'${env:NAME}'}.
            </InfoTip>
          )
        }
      >
        <div className="panel-body">
          <div className="form-grid">
            {server ? (
              <>
                <Text label="Certificate" value={tls.cert_file} onChange={(v) => setTls('cert_file', v)} placeholder="server.pem" />
                <Text label="Key" value={tls.key_file} onChange={(v) => setTls('key_file', v)} placeholder="server.key" />
                <Text label="Client CA" value={tls.client_ca_file} onChange={(v) => setTls('client_ca_file', v)} placeholder="optional: mutual TLS" />
              </>
            ) : (
              <>
                <Text label="CA file" value={tls.ca_file ?? transport.ca_file} onChange={(v) => setTls('ca_file', v)} placeholder="default: system roots" />
                <Text label="Client certificate" value={tls.cert_file} onChange={(v) => setTls('cert_file', v)} placeholder="optional: mutual TLS" />
                <Text label="Client key" value={tls.key_file} onChange={(v) => setTls('key_file', v)} placeholder="with the certificate" />
                <Text label="Server name" value={tls.server_name} onChange={(v) => setTls('server_name', v)} placeholder="default: the host" />
                <label className="field">
                  <Label size="sm">Skip verification</Label>
                  <Toggle
                    size="sm"
                    value={tls.insecure_skip_verify === true}
                    onChange={(skip) => setTls('insecure_skip_verify', skip)}
                    aria-label="Skip certificate verification"
                  />
                </label>
              </>
            )}
          </div>
        </div>
      </CollapsiblePanel>
    </div>
  )
}

/** Transport and codec settings for a source. */
export function TransportForm({
  transport,
  codec,
  onTransport,
  onCodec,
}: {
  transport: Transport
  codec: Codec
  onTransport: (t: Transport) => void
  onCodec: (c: Codec) => void
}) {
  const set = (k: string, v: unknown) => {
    const next = { ...transport, [k]: v }
    if (v === undefined || v === '') delete next[k]
    onTransport(next)
  }
  const [plugins, setPlugins] = useState<PluginInfo[]>([])
  useEffect(() => {
    if (codec.type !== 'plugin') return
    api.plugins().then(setPlugins, () => setPlugins([]))
  }, [codec.type])
  const plugin = plugins.find((p) => p.name === codec.plugin)
  const pluginOptions = (codec.options as Record<string, unknown> | undefined) ?? {}
  const choosePlugin = (name: string) => {
    const p = plugins.find((x) => x.name === name)
    if (!p) return
    onCodec({ type: 'plugin', plugin: p.name, options: structuredClone(p.default_options) })
    // A stream of the plugin's format is framed its way.
    if (p.framing && (transport.type === 'tcp_client' || transport.type === 'tcp_server')) {
      onTransport({ ...transport, framing: structuredClone(p.framing) })
    }
  }
  const framing = (transport.framing as Record<string, unknown> | undefined) ?? { type: 'lines' }
  const setFraming = (k: string, v: unknown) => set('framing', { ...framing, [k]: v })

  return (
    <div className="form-grid">
      <div className="field">
        <Label size="sm">Transport</Label>
        <Select
          ariaLabel="Transport"
          options={TRANSPORTS}
          value={transport.type}
          onChange={(id) => onTransport(structuredClone(TRANSPORTS[id].initial))}
        />
      </div>
      <div className="field">
        <Label size="sm">Codec</Label>
        <Select ariaLabel="Codec" options={CODECS} value={codec.type} onChange={(id) => onCodec(structuredClone(CODECS[id].initial))} />
      </div>
      {codec.type === 'json' && (
        <Text
          label="Record array path"
          value={codec.records}
          placeholder="blank: auto-detect on probe"
          onChange={(v) => {
            const next: Codec = { ...codec, records: v }
            if (!v) delete next.records
            onCodec(next)
          }}
        />
      )}
      {codec.type === 'plugin' && (
        <>
          <div className="field">
            <Label size="sm">Plugin</Label>
            <PluginPicker plugins={plugins} kind="codec" value={codec.plugin as string | undefined} onChange={(p) => choosePlugin(p.name)} width={220} />
          </div>
          <PluginOptions
            plugin={plugin}
            value={pluginOptions}
            onChange={(options) => onCodec({ ...codec, options })}
            row={(key, label, help, control) => (
              <div key={key} className="field" title={help}>
                <Label size="sm">{label}</Label>
                {control}
              </div>
            )}
          />
        </>
      )}
      {codec.type === 'xml' && (
        <Text label="Record element" value={codec.record_element} onChange={(v) => onCodec({ ...codec, record_element: v })} />
      )}

      {(transport.type === 'http_poll' || transport.type === 'websocket') && (
        <Text label="URL" wide value={transport.url} onChange={(v) => set('url', v)} placeholder="https://… or wss://… (${env:NAME} for secrets)" />
      )}
      {transport.type === 'http_poll' && (
        <>
          <Text label="Interval (s)" type="number" value={transport.interval_secs} onChange={(v) => set('interval_secs', num(v))} />
          <Text label="Timeout (s)" type="number" value={transport.timeout_secs} onChange={(v) => set('timeout_secs', num(v))} placeholder="20" />
          <JsonSetting label="Headers" value={transport.headers} onChange={(v) => set('headers', v)} />
        </>
      )}
      {transport.type === 'websocket' && (
        <>
          <Text label="In-band error path" value={transport.error_path} onChange={(v) => set('error_path', v)} placeholder="e.g. error" />
          <Text label="Ping every (s)" type="number" value={transport.ping_secs} onChange={(v) => set('ping_secs', num(v))} placeholder="20" />
          <JsonSetting label="Subscribe message" value={transport.subscribe} onChange={(v) => set('subscribe', v)} />
          <JsonSetting label="Headers" value={transport.headers} onChange={(v) => set('headers', v)} />
        </>
      )}
      {transport.type === 'tcp_client' && (
        <>
          <Text label="Host" value={transport.host} onChange={(v) => set('host', v)} />
          <Text label="Port" type="number" value={transport.port} onChange={(v) => set('port', num(v))} />
          <Text label="Send on connect" value={transport.send_on_connect} onChange={(v) => set('send_on_connect', v)} placeholder="optional" />
        </>
      )}
      {(transport.type === 'tcp_server' || transport.type === 'udp') && (
        <Text label="Bind address" value={transport.bind} onChange={(v) => set('bind', v)} placeholder="0.0.0.0:6969" />
      )}
      {transport.type === 'udp' && (
        <>
          <Text label="Multicast group" value={transport.multicast_group} onChange={(v) => set('multicast_group', v)} placeholder="optional, e.g. 239.2.3.1" />
          <Text label="Multicast interface" value={transport.multicast_interface} onChange={(v) => set('multicast_interface', v)} placeholder="optional" />
        </>
      )}
      {transport.type === 'mqtt' && (
        <>
          <Text
            label="Broker URL"
            wide
            value={transport.url}
            onChange={(v) => set('url', v)}
            placeholder="mqtt://host:1883 or mqtts://host:8883"
          />
          <ListSetting
            label="Topics"
            value={transport.topics}
            onChange={(v) => onTransport({ ...transport, topics: v })}
            placeholder="comma separated; + and # wildcards, e.g. ais/+/position"
          />
          <div className="field">
            <Label size="sm">QoS</Label>
            <Select ariaLabel="QoS" options={QOS} value={String(transport.qos ?? 0)} onChange={(id) => set('qos', Number(id))} />
          </div>
          <Text label="Client id" value={transport.client_id} onChange={(v) => set('client_id', v)} placeholder="default: unique per connection" />
          <Text label="Username" value={transport.username} onChange={(v) => set('username', v)} placeholder="optional" />
          <Text label="Password" value={transport.password} onChange={(v) => set('password', v)} placeholder="${env:NAME}" />
          <Text label="Keepalive (s)" type="number" value={transport.keepalive_secs} onChange={(v) => set('keepalive_secs', num(v))} placeholder="30" />
          <label className="field">
            <Label size="sm">Persistent session</Label>
            <Toggle
              size="sm"
              value={transport.clean_session === false}
              onChange={(persistent) => set('clean_session', persistent ? false : undefined)}
              aria-label="Persistent session"
            />
          </label>
        </>
      )}
      {TLS_TRANSPORTS.includes(transport.type) && <TlsSettings transport={transport} onTransport={onTransport} />}
      {(transport.type === 'tcp_client' || transport.type === 'tcp_server') && (
        <>
          <div className="field">
            <Label size="sm">Framing</Label>
            <Select
              ariaLabel="Framing"
              options={FRAMINGS}
              value={String(framing.type)}
              onChange={(id) => set('framing', structuredClone(FRAMINGS[id].initial))}
            />
          </div>
          {framing.type === 'end_tag' && <Text label="End tag" value={framing.tag} onChange={(v) => setFraming('tag', v)} />}
          {framing.type === 'delimiter' && (
            <Text label="Delimiter" value={framing.delimiter} onChange={(v) => setFraming('delimiter', v)} placeholder="\\n, \\x03 …" />
          )}
          {framing.type === 'length_field' && (
            <>
              <Text label="Length offset (bytes)" type="number" value={framing.offset} onChange={(v) => setFraming('offset', num(v))} />
              <Text label="Length width" value={framing.width} onChange={(v) => setFraming('width', v)} placeholder="u8, u16, u32" />
            </>
          )}
          {framing.type === 'length_prefix' && (
            <Text label="Prefix width" value={framing.width} onChange={(v) => setFraming('width', v)} placeholder="u8, u16, u32, varint" />
          )}
        </>
      )}
    </div>
  )
}
