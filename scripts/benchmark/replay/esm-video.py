#!/usr/bin/env python3
"""Render the esm-crossfix scenario (docs/non-point-contacts.md) as videos:
lines of bearing, ELINT areas, cross-fixes and the tracks they make.

Frames come from the engine's scenario test:
    OT_REPLAY_TRACE=<dir> cargo test -p ot-server esm_crossfix
    OT_ESM_NAIVE=1 OT_REPLAY_TRACE=<dir> cargo test -p ot-server esm_crossfix   # ghost rules off

Usage:
    esm-video.py overview <esm-crossfix.jsonl> <out.mp4>
    esm-video.py ghosts <esm-crossfix-naive.jsonl> <esm-crossfix.jsonl> <out.mp4>
    esm-video.py follow <esm-crossfix.jsonl> <emitter> <out.mp4>
Needs matplotlib, numpy and imageio-ffmpeg.
"""

import json
import math
import sys

import imageio_ffmpeg
import matplotlib
import numpy as np

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.patches import Circle  # noqa: E402

ORIGIN = (50.6, -1.4)
R = 6371000.0
FPS, PER_STEP = 10, 2  # two frames per 5 s step: 25x speed
BG, FG, MUTED, GRID = "#0f1419", "#e6e8eb", "#6b7580", "#1f2730"
GHOST, FIXC, WAIT = "#ff5d5d", "#f2f2f2", "#56606a"
PALETTE = ["#f5a524", "#3fb8f0", "#b07cf2", "#6fd08c", "#f06b8a", "#c9c26b", "#8a9bf5", "#e0895c", "#5cc9c0", "#d68ad6",
           "#ffcf5c", "#7fd4ff", "#cfa3ff", "#9be8b0", "#ff9fb5", "#e6df8a", "#b3beff", "#ffb48a", "#8ae6de", "#f0b3f0"]
RANGE_M = 60000.0


def xy(lat, lon):
    """East, north in metres from the scenario's origin (flat earth is plenty at 60 km)."""
    return (math.radians(lon - ORIGIN[1]) * R * math.cos(math.radians(ORIGIN[0])), math.radians(lat - ORIGIN[0]) * R)


def load(path):
    rows = [json.loads(line) for line in open(path) if line.strip()]
    header, frames = rows[0], rows[1:]
    # ELINT areas come every 30 s: keep showing each, faded, until the next.
    last = {}
    for f in frames:
        for a in f["areas"]:
            last[a["emitter"]] = (f["t"], a)
        fresh = {a["emitter"] for a in f["areas"]}
        for em, (t, a) in last.items():
            if em not in fresh and f["t"] - t < 30:
                f["areas"].append({**a, "stale": True})
    return header, frames


class Colours:
    def __init__(self):
        self.of = {}

    def __call__(self, uid):
        if uid is None:
            return MUTED
        if uid not in self.of:
            self.of[uid] = PALETTE[len(self.of) % len(PALETTE)]
        return self.of[uid]


def is_ghost(fix, ships):
    fx, fy = xy(fix["lat"], fix["lon"])
    best = min(math.hypot(fx - x, fy - y) for x, y in (xy(*s) for s in ships))
    return best > 3 * max(fix["sigma"], 300.0)


def label(t):
    for i in t["ids"]:
        scheme, value = i.split(":", 1)
        if scheme == "mmsi":
            return f"MMSI …{value[-2:]}"
    for i in t["ids"]:
        scheme, value = i.split(":", 1)
        if scheme == "elnot":
            return value
    return "fix" if set(t["sources"]) == {"fix"} else t["uid"][-3:]


def style(ax, xlim, ylim, grid=10000):
    ax.cla()
    ax.set_facecolor(BG)
    ax.set_xlim(*xlim)
    ax.set_ylim(*ylim)
    ax.set_aspect("equal")
    ax.set_xticks([])
    ax.set_yticks([])
    for sp in ax.spines.values():
        sp.set_visible(False)
    for g in np.arange(math.floor(xlim[0] / grid) * grid, xlim[1] + grid, grid):
        ax.axvline(g, color=GRID, lw=0.6, zorder=0)
    for g in np.arange(math.floor(ylim[0] / grid) * grid, ylim[1] + grid, grid):
        ax.axhline(g, color=GRID, lw=0.6, zorder=0)


def draw(ax, header, frame, colour, sub, *, only_anonymous=False, show_tracks=True, focus=None, text_scale=1.0):
    """One step: sensors, truth, lines by fate, areas, fixes and tracks. `sub` is
    0 on the step's first frame (lines bright) and then fades."""
    fade = 1.0 if sub == 0 else 0.55
    for i, (lat, lon) in enumerate(header["sensors"]):
        x, y = xy(lat, lon)
        ax.scatter([x], [y], marker="^", s=110 * text_scale, color=FG, zorder=7)
        ax.text(x + 900, y - 2600, f"ESM {i + 1}", color=FG, fontsize=9 * text_scale, zorder=7)
    # Truth: faint rings, AIS ships marked.
    for i, (lat, lon) in enumerate(frame["ships"]):
        x, y = xy(lat, lon)
        hot = focus is None or i == focus
        ax.add_patch(Circle((x, y), 700, fill=False, ec=FG, alpha=0.35 if hot else 0.15, lw=1, zorder=1))
        ax.text(x + 800, y + 500, f"#{i}", color=MUTED, fontsize=7 * text_scale, alpha=0.9 if hot else 0.5, zorder=1)
    # Lines of bearing, coloured by where they went.
    for ln in frame["lines"]:
        if only_anonymous and ln["id"]:
            continue
        if focus is not None and int(ln["emitter"]) != focus:
            continue
        x0, y0 = xy(ln["lat"], ln["lon"])
        b = math.radians(ln["bearing"])
        x1, y1 = x0 + RANGE_M * math.sin(b), y0 + RANGE_M * math.cos(b)
        if ln["fate"] == "track":
            c, a, w, ls = colour(ln["uid"]), 0.55, 0.9, "-"
        elif ln["fate"] == "fix":
            c, a, w, ls = FIXC, 0.5, 0.8, "-"
        else:
            c, a, w, ls = WAIT, 0.45, 0.7, (0, (4, 3))
        ax.plot([x0, x1], [y0, y1], color=c, alpha=a * fade, lw=w, ls=ls, zorder=2)
    # ELINT areas: the colour of the track they paired with.
    if not only_anonymous:
        for ar in frame["areas"]:
            if focus is not None and int(ar["emitter"]) != focus:
                continue
            x, y = xy(ar["lat"], ar["lon"])
            c = colour(ar["uid"])
            k = 0.4 if ar.get("stale") else fade
            ax.add_patch(Circle((x, y), ar["r"], fill=True, fc=c, alpha=0.08 * k, ec="none", zorder=2))
            ax.add_patch(Circle((x, y), ar["r"], fill=False, ec=c, alpha=0.8 * k, lw=1.2, ls="--", zorder=2))
    # Fixes made this step: 1-sigma circle; a ghost (no emitter within 3 sigma) in red.
    for fx in frame["fixes"]:
        if only_anonymous and not anonymous(fx):
            continue
        x, y = xy(fx["lat"], fx["lon"])
        if focus is not None and not any(m.endswith(f"/em{focus}") for m in fx["members"]):
            continue
        g = is_ghost(fx, frame["ships"])
        c = GHOST if g else FIXC
        ax.add_patch(Circle((x, y), fx["sigma"], fill=False, ec=c, alpha=0.9 * fade, lw=1.4 if g else 1.0, zorder=4))
        ax.scatter([x], [y], marker="x" if g else "+", s=60 * text_scale, color=c, alpha=fade, lw=1.6, zorder=4)
    if not show_tracks:
        return
    for t in frame["tracks"]:
        x, y = xy(t["lat"], t["lon"])
        c = colour(t["uid"])
        src = set(t["sources"])
        m = "s" if "ais" in src else ("D" if "elint" in src else "o")
        size = (110 if "ais" in src else 80) * text_scale
        if t["confirmed"]:
            ax.scatter([x], [y], marker=m, s=size, color=c, edgecolors=BG, lw=0.8, zorder=6)
        else:
            ax.scatter([x], [y], marker=m, s=size, facecolors="none", edgecolors=c, lw=1.4, zorder=6)
        ax.text(x + 900, y - 1600, label(t), color=c, fontsize=8 * text_scale, weight="bold", zorder=6)


def anonymous(fx):
    """A fix of lines without an emitter identity (the scenario gives odd emitters none)."""
    return int(fx["members"][0].rsplit("/em", 1)[1]) % 2 == 1


def counters(frames, upto, only_anonymous=False):
    fixes = ghosts = to_track = 0
    for f in frames[: upto + 1]:
        for fx in f["fixes"]:
            if only_anonymous and not anonymous(fx):
                continue
            fixes += 1
            ghosts += is_ghost(fx, f["ships"])
        to_track += sum(1 for ln in f["lines"] if ln["fate"] == "track")
    return fixes, ghosts, to_track


def legend(ax, items, x=0.02, y0=0.05, dy=0.04, scale=1.0):
    y = y0 + dy * (len(items) - 1)
    for kind, text, c in items:
        if kind == "line":
            ax.plot([x - 0.008, x + 0.012], [y, y], transform=ax.transAxes, color=c, lw=1.4)
        elif kind == "dash":
            ax.plot([x - 0.008, x + 0.012], [y, y], transform=ax.transAxes, color=c, lw=1.2, ls=(0, (3, 2)))
        else:
            ax.scatter([x + 0.002], [y], transform=ax.transAxes, marker=kind, s=40 * scale, color=c)
        ax.text(x + 0.022, y, text, transform=ax.transAxes, color=FG, fontsize=9 * scale, va="center")
        y -= dy


class Video:
    def __init__(self, out):
        self.fig = plt.figure(figsize=(12.8, 7.2), dpi=100, facecolor=BG)
        self.w = imageio_ffmpeg.write_frames(out, (1280, 720), fps=FPS, quality=5, macro_block_size=8)
        self.w.send(None)
        self.out = out

    def send(self):
        self.fig.canvas.draw()
        self.w.send(np.asarray(self.fig.canvas.buffer_rgba())[:, :, :3].tobytes())

    def close(self):
        self.w.close()
        print(self.out)


BOX = 42000  # half-width of the square map, metres


def overview(path, out):
    header, frames = load(path)
    colour = Colours()
    v = Video(out)
    ax = v.fig.add_axes([0.0, 0.0, 720 / 1280, 1.0])
    side = v.fig.add_axes([720 / 1280, 0.0, 560 / 1280, 1.0])
    for k, f in enumerate(frames):
        fixes, ghosts, to_track = counters(frames, k)
        tracked = sum(1 for t in f["tracks"] if t["confirmed"])
        for sub in range(PER_STEP):
            style(ax, (-BOX, BOX), (-BOX, BOX))
            draw(ax, header, f, colour, sub)
            ax.text(0.99, 0.01, "grid 10 km", transform=ax.transAxes, color=MUTED, fontsize=8, ha="right")
            side.cla()
            side.set_facecolor(BG)
            side.axis("off")
            side.set_xlim(0, 1)
            side.set_ylim(0, 1)
            side.text(0.04, 0.95, "esm-crossfix · the whole picture", color=FG, fontsize=15, weight="bold", va="top")
            side.text(0.04, 0.895, f"t = {f['t']:3d} s   ×25 speed", color=MUTED, fontsize=10, family="monospace", va="top")
            side.text(0.04, 0.84, "20 ships with radars, 10 on AIS. 4 ESM sensors report a bearing\n"
                      "to each emitter in range every 5 s (1.5° noise); half carry the\n"
                      "emitter's ELNOT. An ELINT source reports 3 km areas for 5 ships.",
                      color=FG, fontsize=9.5, va="top", linespacing=1.5)
            stats = [
                ("bearings to a track", f"{to_track}"),
                ("cross-fixes", f"{fixes}"),
                ("ghost fixes", f"{ghosts}  ({100 * ghosts / max(fixes, 1):.1f}%)"),
                ("confirmed tracks", f"{tracked}"),
            ]
            y = 0.66
            for name, value in stats:
                side.text(0.04, y, name, color=MUTED, fontsize=10, va="top")
                side.text(0.55, y, value, color=GHOST if name == "ghost fixes" and ghosts else FG, fontsize=10, family="monospace", va="top")
                y -= 0.045
            legend(side, [
                ("line", "bearing that went to a track (its colour)", PALETTE[1]),
                ("line", "bearing used in a cross-fix", FIXC),
                ("dash", "bearing waiting (for a third sensor or a repeat)", WAIT),
                ("+", "cross-fix, circle = 1σ", FIXC),
                ("x", "ghost fix: no ship within 3σ", GHOST),
                ("s", "track with AIS", FG),
                ("D", "track with ELINT areas", FG),
                ("o", "track from cross-fixes only (hollow: tentative)", FG),
                ("^", "ESM sensor", FG),
            ], x=0.06, y0=0.06, dy=0.042)
            side.add_patch(Circle((0.06, 0.465), 0.012, transform=side.transAxes, fill=False, ec=FG, alpha=0.5))
            side.text(0.082, 0.465, "true ship position", transform=side.transAxes, color=FG, fontsize=9, va="center")
            v.send()
    v.close()


def ghosts(naive_path, path, out):
    header, off = load(naive_path)
    _, on = load(path)
    v = Video(out)
    axes = [v.fig.add_axes([0.0, 0.0, 0.5, 0.86]), v.fig.add_axes([0.5, 0.0, 0.5, 0.86])]
    top = v.fig.add_axes([0.0, 0.86, 1.0, 0.14])
    ca, cb = Colours(), Colours()
    for k in range(min(len(off), len(on))):
        stats = [counters(off, k, True), counters(on, k, True)]
        for sub in range(PER_STEP):
            for ax, frames, colour, title, (fixes, gh, _) in zip(
                axes, (off, on), (ca, cb), ("Consensus only: three sensors agree", "With the ghost rules"), stats
            ):
                style(ax, (-BOX, BOX), (-BOX, BOX))
                draw(ax, header, frames[k], colour, sub, only_anonymous=True, show_tracks=False, text_scale=0.8)
                box = {"facecolor": BG, "edgecolor": "none", "alpha": 0.85, "pad": 2}
                ax.text(0.5, 0.985, title, transform=ax.transAxes, color=FG, fontsize=12, weight="bold", va="top", ha="center", bbox=box)
                ax.text(0.5, 0.93, f"fixes {fixes}   ghosts {gh} ({100 * gh / max(fixes, 1):.1f}%)", ha="center",
                        transform=ax.transAxes, color=GHOST if gh else FG, fontsize=10, family="monospace", va="top", bbox=box)
            top.cla()
            top.set_facecolor(BG)
            top.axis("off")
            top.set_xlim(0, 1)
            top.set_ylim(0, 1)
            top.text(0.012, 0.78, "Ghost fixes: anonymous bearings only (no ELNOT)", color=FG, fontsize=15, weight="bold", va="top")
            top.text(0.012, 0.36, f"t = {off[k]['t']:3d} s  ×25   ·   white + = fix near a ship   ·   red × = ghost, no ship within 3σ   ·   "
                     "dashed grey = waiting   ·   rings = true positions", color=MUTED, fontsize=9.5, va="top")
            v.send()
    v.close()


def follow(path, emitter, out):
    header, frames = load(path)
    colour = Colours()
    v = Video(out)
    ax = v.fig.add_axes([0.0, 0.0, 1.0, 1.0])
    half = 14000
    cx = cy = None
    events = []  # (step, text)
    seen_uid = None
    for k, f in enumerate(frames):
        sx, sy = xy(*f["ships"][emitter])
        # Follow smoothly.
        cx = sx if cx is None else cx + 0.3 * (sx - cx)
        cy = sy if cy is None else cy + 0.3 * (sy - cy)
        mine = [ln for ln in f["lines"] if int(ln["emitter"]) == emitter]
        to_track = [ln for ln in mine if ln["fate"] == "track"]
        fixes = [fx for fx in f["fixes"] if any(m.endswith(f"/em{emitter}") for m in fx["members"])]
        if fixes and not any("first cross-fix" in e[1] for e in events):
            events.append((k, "first cross-fix: three sensors agree twice"))
        area = [a for a in f["areas"] if int(a["emitter"]) == emitter]
        if area and area[0]["uid"] and not any("ELINT area" in e[1] for e in events):
            events.append((k, "ELINT area pairs with the track"))
        if to_track and not any("go to its track" in e[1] for e in events):
            seen_uid = to_track[0]["uid"]
            events.append((k, "the emitter's bearings now go to its track directly"))
        for sub in range(PER_STEP):
            style(ax, (cx - half * 16 / 9, cx + half * 16 / 9), (cy - half, cy + half), grid=2000)
            draw(ax, header, f, colour, sub, focus=emitter)
            ax.text(0.012, 0.975, f"esm-crossfix · following ship #{emitter}", transform=ax.transAxes, color=FG, fontsize=15, weight="bold", va="top")
            kinds = []
            kinds.append("on AIS" if emitter < header["ais"] else "no AIS")
            kinds.append("bearings carry its ELNOT" if emitter % 2 == 0 else "anonymous bearings")
            if emitter >= header["elint_from"]:
                kinds.append("ELINT areas")
            ax.text(0.012, 0.925, f"t = {f['t']:3d} s  ×25   ·   " + " · ".join(kinds), transform=ax.transAxes, color=MUTED, fontsize=10, family="monospace", va="top")
            ax.text(0.012, 0.885, f"this step: {len(mine)} bearings, {len(to_track)} to a track, {len(fixes)} fix",
                    transform=ax.transAxes, color=FG, fontsize=10, family="monospace", va="top")
            by = 0.975
            for step, text in events:
                if 0 <= k - step <= 8:
                    ax.text(0.99, by, text, transform=ax.transAxes, color=FG, fontsize=11, ha="right", va="top", weight="bold",
                            bbox={"facecolor": BG, "edgecolor": colour(seen_uid) if seen_uid else FG, "boxstyle": "round,pad=0.35", "alpha": 0.9})
                    by -= 0.06
            ax.text(0.99, 0.02, "only this ship's bearings, areas and fixes · ring = true position · grid 2 km",
                    transform=ax.transAxes, color=MUTED, fontsize=9, ha="right")
            v.send()
    v.close()


if __name__ == "__main__":
    cmd, *rest = sys.argv[1:]
    if cmd == "overview":
        overview(*rest)
    elif cmd == "ghosts":
        ghosts(*rest)
    elif cmd == "follow":
        follow(rest[0], int(rest[1]), rest[2])
    else:
        sys.exit(__doc__)
