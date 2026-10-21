import type { ReactNode } from 'react'
import {
  TbAddressBook,
  TbApi,
  TbArrowsExchange,
  TbBan,
  TbBinary,
  TbBraces,
  TbBroadcast,
  TbClockPause,
  TbCode,
  TbFile,
  TbFilter,
  TbFlag,
  TbLink,
  TbMessages,
  TbNetwork,
  TbPlugConnected,
  TbPuzzle,
  TbRadar,
  TbRoute,
  TbSend,
  TbServer,
  TbTargetArrow,
  TbWorldDownload,
} from 'react-icons/tb'
import type { SourceSpec } from '../../../api/client'
import type { StepId } from './sourceFlow'

const TRANSPORT_ICONS: Record<string, ReactNode> = {
  tcp_client: <TbNetwork />,
  tcp_server: <TbServer />,
  udp: <TbBroadcast />,
  http_poll: <TbWorldDownload />,
  websocket: <TbPlugConnected />,
  mqtt: <TbMessages />,
  grpc_client: <TbApi />,
  grpc_server: <TbArrowsExchange />,
  file: <TbFile />,
}

const CODEC_ICONS: Record<string, ReactNode> = {
  json: <TbBraces />,
  cot_xml: <TbTargetArrow />,
  xml: <TbCode />,
  protobuf: <TbBinary />,
  plugin: <TbPuzzle />,
}

const STAGE_ICONS: Partial<Record<StepId, ReactNode>> = {
  reject: <TbBan />,
  map: <TbRoute />,
  join: <TbLink />,
  registry: <TbAddressBook />,
  affiliation: <TbFlag />,
  filter: <TbFilter />,
  tracker: <TbRadar />,
  throttle: <TbClockPause />,
  output: <TbSend />,
}

/** A step's icon, for the source as it is. */
export function stepIcon(spec: SourceSpec, step: StepId): ReactNode {
  if (step === 'source') return TRANSPORT_ICONS[spec.transport.type]
  if (step === 'decode') return CODEC_ICONS[spec.pipeline.codec.type]
  return STAGE_ICONS[step]
}

/** A palette item's icon, from its type (`source:udp`, `decode:plugin:x`, `stage:filter`, `output:tracks`). */
export function entryIcon(type: string): ReactNode {
  const [kind, sub] = type.split(':')
  if (kind === 'source') return TRANSPORT_ICONS[sub]
  if (kind === 'decode') return CODEC_ICONS[sub]
  if (kind === 'stage') return STAGE_ICONS[sub as StepId]
  return STAGE_ICONS.output
}
