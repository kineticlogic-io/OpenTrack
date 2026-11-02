import { createContext, useContext } from 'react'
import type { MapTiles } from '@kineticlogic/staresdk/map-view'

export type BasemapTilesFormat = 'raster' | 'vector'
export type BasemapTiles = MapTiles & { format: BasemapTilesFormat }

/** The maps' basemap tiles, fetched through OpenTrack (Settings → General: Basemap tiles). */
const BASEMAP_TILES: MapTiles = { url: '/api/v1/basemap/{z}/{x}/{y}', maxZoom: 19 }

/** The active basemap representation, from the settings the header already loads. */
export const BasemapCtx = createContext<BasemapTilesFormat | null>(null)

/** The `tiles` prop for a MapView: the proxied tiles when they are on, else none (the outlines). */
export function useBasemapTiles(): BasemapTiles | undefined {
  const format = useContext(BasemapCtx)
  return format ? { ...BASEMAP_TILES, format } : undefined
}
