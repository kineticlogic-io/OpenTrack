import type { ReactNode } from 'react'
import { Handle, Position, type NodeProps } from '@xyflow/react'
import { KIND_WORD, type StepKind } from './sourceFlow'

/** What the editor gives each step to draw (the canvas node's `data`). */
export interface StepView extends Record<string, unknown> {
  kind: StepKind
  icon: ReactNode
  title: string
  summary: string
  problems: string[]
}

/**
 * One step of a source on the canvas: its icon, heading and one line of its settings, joined to the
 * step before and after it, with what stops it running in red.
 */
export function StepNode({ data, selected }: NodeProps) {
  const step = data as StepView
  const failing = step.problems.length > 0
  return (
    <div
      className="flow-step"
      data-failing={failing || undefined}
      data-selected={selected || undefined}
      style={{
        border: `1px solid ${failing ? 'var(--color-destructive)' : selected ? 'var(--color-accent)' : 'var(--border-strong)'}`,
      }}
    >
      {step.kind !== 'source' && <Handle type="target" position={Position.Top} isConnectable={false} />}
      <div className="flow-step-head">
        <span aria-hidden className="flow-step-icon">
          {step.icon}
        </span>
        <span className="flow-step-title">{step.title}</span>
        <span className="flow-step-kind">{KIND_WORD[step.kind]}</span>
      </div>
      {step.summary && <span className="flow-step-summary">{step.summary}</span>}
      {step.problems.map((message) => (
        <span key={message} role="alert" className="flow-step-problem">
          {message}
        </span>
      ))}
      {step.kind !== 'output' && <Handle type="source" position={Position.Bottom} isConnectable={false} />}
    </div>
  )
}
