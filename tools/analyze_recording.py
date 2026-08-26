#!/usr/bin/env python3
"""Print A/V duration, frame count, and implied fps for an ALVR recording MKV."""

from __future__ import annotations

import json
import subprocess
import sys


def probe(path: str) -> dict:
    cmd = [
        "ffprobe",
        "-hide_banner",
        "-show_format",
        "-show_streams",
        "-count_frames",
        "-of",
        "json",
        path,
    ]
    out = subprocess.check_output(cmd, stderr=subprocess.STDOUT, text=True)
    # ffprobe mixes banner into stdout when we merge; keep JSON object only.
    start = out.find("{")
    if start < 0:
        raise RuntimeError(out)
    return json.loads(out[start:])


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: python tools/analyze_recording.py <recording.mkv>")
        return 2
    path = sys.argv[1]
    data = probe(path)
    fmt = data.get("format", {})
    print(f"file={path}")
    print(f"container_duration_s={fmt.get('duration')}")
    video_frames = None
    audio_s = None
    video_s = None
    for s in data.get("streams", []):
        kind = s.get("codec_type")
        dur = s.get("tags", {}).get("DURATION") or s.get("duration")
        frames = s.get("nb_read_frames") or s.get("nb_frames")
        print(f"stream={kind} codec={s.get('codec_name')} r={s.get('r_frame_rate')} avg={s.get('avg_frame_rate')} start={s.get('start_time')} duration={dur} frames={frames}")
        if kind == "video":
            video_frames = int(frames) if frames and str(frames).isdigit() else None
            if dur and ":" in str(dur):
                h, m, sec = str(dur).split(":")
                video_s = float(h) * 3600 + float(m) * 60 + float(sec)
            elif dur:
                video_s = float(dur)
        if kind == "audio":
            if dur and ":" in str(dur):
                h, m, sec = str(dur).split(":")
                audio_s = float(h) * 3600 + float(m) * 60 + float(sec)
            elif dur:
                audio_s = float(dur)
    if video_frames and audio_s and audio_s > 0.05:
        implied = video_frames / audio_s
        print(f"implied_fps_if_video_matches_audio={implied:.4f}")
        if video_s:
            print(f"duration_delta_audio_minus_video_s={audio_s - video_s:.4f}")
            print(f"if_labeled_30fps_video_s={video_frames / 30.0:.4f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
