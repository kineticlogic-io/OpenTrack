import { useEffect, useRef, useState } from 'react'
import { api, type PreviewResult, type SourceSpec } from '../api/client'
import { errorMessage } from './format'

const PREVIEW_DELAY_MS = 600

/**
 * A dry run of `spec` over `sampleSourceId`'s stored probe samples, re-run shortly after the spec
 * stops changing. Nothing is saved or published.
 */
export function usePreview(spec: SourceSpec, sampleSourceId: string, nonce = 0) {
  const [preview, setPreview] = useState<PreviewResult | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [running, setRunning] = useState(false)
  const seq = useRef(0)

  useEffect(() => {
    const id = ++seq.current
    const t = setTimeout(() => {
      setRunning(true)
      api
        .preview(spec, sampleSourceId)
        .then((r) => {
          if (id !== seq.current) return
          setPreview(r)
          setError(null)
        })
        .catch((e) => {
          if (id !== seq.current) return
          setError(errorMessage(e))
        })
        .finally(() => id === seq.current && setRunning(false))
    }, PREVIEW_DELAY_MS)
    return () => clearTimeout(t)
  }, [spec, sampleSourceId, nonce])

  return { preview, error, running }
}
