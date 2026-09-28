import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { TbSearch } from 'react-icons/tb'
import { CollapsiblePanel, Input, MDText, Tabs, Tree, type Components, type TreeNode } from 'staresdk'
import { GUIDES, guideHeadings, helpDomId, helpHref, isGuide, parseHelpPath, renderableGuide, type GuideHeading, type GuideId } from '../../lib/help'
import { guideMarkdown } from './guides'
import './help.css'

const NO_CHECKS = new Set<string>()

/** Sections (level 2) with their sub-sections (level 3) as tree nodes; the title is left out. */
function sectionTree(headings: GuideHeading[], query: string): TreeNode[] {
  const q = query.trim().toLowerCase()
  const nodes: TreeNode[] = []
  for (const h of headings) {
    if (h.level === 2) nodes.push({ id: h.anchor, title: h.text, children: [] })
    else if (h.level === 3 && nodes.length) nodes[nodes.length - 1].children!.push({ id: h.anchor, title: h.text })
  }
  const tidy = (n: TreeNode): TreeNode => (n.children?.length ? n : { id: n.id, title: n.title })
  if (!q) return nodes.map(tidy)
  const hit = (n: TreeNode) => n.title.toLowerCase().includes(q)
  return nodes
    .map((n) => (hit(n) ? n : { ...n, children: (n.children ?? []).filter(hit) }))
    .filter((n) => hit(n) || (n.children ?? []).length > 0)
    .map(tidy)
}

/** Where a link in a guide goes: a section here, the other guide, the repository, or the web. */
function resolveLink(guide: GuideId, href: string): { kind: 'help'; href: string } | { kind: 'repo'; path: string } | { kind: 'web'; href: string } {
  if (href.startsWith('#')) return { kind: 'help', href: helpHref(guide, href.slice(1)) }
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) return { kind: 'web', href }
  const [path, anchor] = href.split('#')
  const file = path.replace(/^\.\//, '')
  const other = GUIDES.find((g) => g.file === file)
  if (other) return { kind: 'help', href: helpHref(other.id, anchor) }
  // Relative to docs/guides/: a file in the repository, which the app does not serve.
  const parts = ['docs', 'guides']
  for (const p of path.split('/')) {
    if (p === '..') parts.pop()
    else if (p && p !== '.') parts.push(p)
  }
  return { kind: 'repo', path: parts.join('/') }
}

/**
 * Help: the operator and administrator guides (docs/guides/*.md, bundled at build time), with a
 * section list. `#help/<guide>/<anchor>` opens a guide at a section; see `helpHref`.
 */
export default function HelpPage({ sub }: { sub: string }) {
  const parsed = parseHelpPath(sub)
  const guide: GuideId = parsed.guide ?? 'operator'
  const anchor = parsed.anchor
  const markdown = guideMarkdown(guide)
  const headings = useMemo(() => (markdown ? guideHeadings(markdown) : []), [markdown])
  const byLine = useMemo(() => new Map(headings.map((h) => [h.line, h])), [headings])
  const [query, setQuery] = useState('')
  // Sections the reader opened (true) or closed (false) in the list; others follow the anchor.
  const [toggled, setToggled] = useState<Map<string, boolean>>(() => new Map())

  // The section holding the current anchor opens in the list.
  const parent = useMemo(() => {
    let section: string | null = null
    for (const h of headings) {
      if (h.level <= 2) section = h.level === 2 ? h.anchor : null
      if (h.anchor === anchor) return section
    }
    return null
  }, [headings, anchor])
  // Scroll to the section the address names (or to the top of a guide), once it is on the page.
  useEffect(() => {
    const id = anchor ? helpDomId(guide, anchor) : `help-${guide}-top`
    const frame = requestAnimationFrame(() => document.getElementById(id)?.scrollIntoView({ block: 'start' }))
    return () => cancelAnimationFrame(frame)
  }, [guide, anchor, markdown])

  const components: Components = useMemo(() => {
    const heading =
      (Tag: 'h1' | 'h2' | 'h3' | 'h4') =>
      ({ node, children }: { node?: { position?: { start: { line: number } } }; children?: ReactNode }) => {
        const h = node?.position ? byLine.get(node.position.start.line) : undefined
        return (
          <Tag id={h ? helpDomId(guide, h.anchor) : undefined} className="help-heading">
            {children}
            {h && Tag !== 'h1' && (
              <a className="help-anchor" href={helpHref(guide, h.anchor)} aria-label={`Link to ${h.text}`} title="Link to this section">
                #
              </a>
            )}
          </Tag>
        )
      }
    return {
      h1: heading('h1'),
      h2: heading('h2'),
      h3: heading('h3'),
      h4: heading('h4'),
      a: ({ href = '', children }) => {
        const to = resolveLink(guide, href)
        if (to.kind === 'help') return <a href={to.href}>{children}</a>
        if (to.kind === 'web')
          return (
            <a href={to.href} target="_blank" rel="noreferrer">
              {children}
            </a>
          )
        return (
          <span className="help-repo-link" title={`In the OpenTrack repository: ${to.path}`}>
            {children}
          </span>
        )
      },
      table: ({ children }) => (
        <div className="help-table">
          <table>{children}</table>
        </div>
      ),
    }
  }, [guide, byLine])

  const tree = useMemo(() => sectionTree(headings, query), [headings, query])
  const shownExpanded = useMemo(() => {
    if (query.trim()) return new Set(tree.map((n) => n.id))
    return new Set(tree.map((n) => n.id).filter((id) => toggled.get(`${guide}/${id}`) ?? id === parent))
  }, [query, tree, toggled, parent, guide])
  const go = (g: GuideId, a?: string) => {
    const to = helpHref(g, a)
    // The same address again: no hashchange, so scroll here.
    if (window.location.hash === to) document.getElementById(a ? helpDomId(g, a) : `help-${g}-top`)?.scrollIntoView({ block: 'start' })
    else window.location.hash = to
  }
  const title = GUIDES.find((g) => g.id === guide)!.label

  return (
    <div className="panels">
      <Tabs aria-label="Guides" value={guide} onChange={(id) => isGuide(id) && go(id)} tabs={GUIDES.map((g) => ({ id: g.id, label: g.label }))} />
      {markdown === null ? (
        <CollapsiblePanel title={title}>
          <div className="panel-body">
            <span className="muted">
              This build of OpenTrack has no guides. They are in the OpenTrack repository, in docs/guides/{GUIDES.find((g) => g.id === guide)!.file}.
            </span>
          </div>
        </CollapsiblePanel>
      ) : (
        <div className="help-layout">
          <nav className="help-nav" aria-label={`${title} sections`}>
            <CollapsiblePanel
              title="Contents"
              titleActions={
                <div className="search">
                  <TbSearch aria-hidden />
                  <Input
                    aria-label="Find a section"
                    placeholder="Find"
                    style={{ paddingLeft: 26 }}
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </div>
              }
            >
              <div className="panel-body help-tree">
                {tree.length ? (
                  <Tree
                    nodes={tree}
                    selectable="single"
                    checkedIds={NO_CHECKS}
                    onCheckedChange={() => {}}
                    expandedIds={shownExpanded}
                    onExpandedChange={(id, open) => setToggled((m) => new Map(m).set(`${guide}/${id}`, open))}
                    selectedId={anchor || null}
                    onSelect={(id) => go(guide, id)}
                  />
                ) : (
                  <span className="muted">No section matches.</span>
                )}
              </div>
            </CollapsiblePanel>
          </nav>
          <article className="help-doc" id={`help-${guide}-top`}>
            <CollapsiblePanel title={title}>
              <div className="panel-body">
                <MDText markdown={renderableGuide(markdown)} components={components} />
              </div>
            </CollapsiblePanel>
          </article>
        </div>
      )}
    </div>
  )
}
