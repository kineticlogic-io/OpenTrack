/**
 * The Cursor-on-Target type for a 2525D SIDC. A CoT type spells out the 2525C warfighting SIDC
 * (SFGPUCI -> a-f-G-U-C-I), so the symbol's 2525C battle dimension and function come from JMSML's
 * 2525D->2525C crosswalk (crosswalk.json, scripts/vendor-symbol-crosswalk.py). A symbol with no
 * 2525C equivalent gets its affiliation and dimension only (a-f-G).
 */
import crosswalk from './crosswalk.json'
import { parseSidc, type Affiliation } from './sidc'

const ATOM: Record<Affiliation, string> = { unknown: 'u', friend: 'f', neutral: 'n', hostile: 'h' }

/** The 2525C battle dimension a 2525D symbol set draws in, when it has no crosswalk entry. */
const DIMENSION: Record<string, string> = {
  '01': 'A', '02': 'A', '05': 'P', '06': 'P',
  '10': 'G', '11': 'G', '15': 'G', '20': 'G', '40': 'G',
  '30': 'S', '35': 'U', '36': 'U',
}

const TABLE = crosswalk as Record<string, string>

/** The CoT type for a 2525D SIDC, or null when it is not one. */
export function sidcToCot(sidc: string): string | null {
  const p = parseSidc(sidc)
  if (!p) return null
  const legacy = TABLE[p.symbolSet + p.code]
  const [dimension, func = ''] = legacy ? legacy.split('-') : [DIMENSION[p.symbolSet] ?? 'X']
  return ['a', ATOM[p.affiliation], dimension, ...func.split('')].join('-')
}
