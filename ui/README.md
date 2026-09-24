# OpenTrack UI

React + TypeScript (Vite) on openstare's stareSDK components. See the repository README for
running it; `npm run dev` proxies `/api` to `opentrack serve` on :8090 (override with
`OT_API_URL`).

Rules: reusable components come only from `staresdk` (vendored in `vendor/`); page layout uses
stareSDK tokens (`var(--…)`) only; icons come only from `react-icons/tb`. A missing component is
proposed for stareSDK upstream rather than built here.
