#!/usr/bin/env python3
import argparse
import json
import math
import os
import subprocess
import sys
import tempfile

import msgpack


ANCHOR_ALIGNMENT = {
    "bottom-left": 1,
    "bottom": 2,
    "bottom-right": 3,
    "left": 4,
    "center": 5,
    "right": 6,
    "top-left": 7,
    "top": 8,
    "top-right": 9,
}


KEY_NAME_MAP = {
    "Return": "Enter",
    "Escape": "Esc",
    "Backspace": "Backspace",
    "Space": "Space",
    "Tab": "Tab",
    "ShiftLeft": "Shift",
    "ShiftRight": "Shift",
    "ControlLeft": "Ctrl",
    "ControlRight": "Ctrl",
    "Alt": "Alt",
    "AltGr": "AltGr",
    "MetaLeft": "Meta",
    "MetaRight": "Meta",
    "UpArrow": "Up",
    "DownArrow": "Down",
    "LeftArrow": "Left",
    "RightArrow": "Right",
    "LeftBracket": "[",
    "RightBracket": "]",
    "Minus": "-",
    "Equal": "=",
    "BackQuote": "`",
    "SemiColon": ";",
    "Quote": "'",
    "BackSlash": "\\",
    "Comma": ",",
    "Dot": ".",
    "Slash": "/",
    "PrintScreen": "PrintScreen",
    "ScrollLock": "ScrollLock",
    "Pause": "Pause",
    "NumLock": "NumLock",
    "PageUp": "PageUp",
    "PageDown": "PageDown",
    "Home": "Home",
    "End": "End",
    "Insert": "Insert",
    "Delete": "Delete",
    "KpReturn": "KPEnter",
    "KpMinus": "KP-",
    "KpPlus": "KP+",
    "KpMultiply": "KP*",
    "KpDivide": "KP/",
    "KpDelete": "KPDel",
}


def normalize_key_name(name: str) -> str:
    if name in KEY_NAME_MAP:
        return KEY_NAME_MAP[name]
    if name.startswith("Key") and len(name) == 4:
        return name[3:]
    if name.startswith("Num") and len(name) == 4:
        return name[3:]
    if name.startswith("Kp") and len(name) == 3:
        return "KP" + name[2:]
    return name


def escape_ass(text: str) -> str:
    return text.replace("\\", r"\\").replace("{", r"\{").replace("}", r"\}")


def format_ass_time(seconds: float) -> str:
    if seconds < 0:
        seconds = 0
    total_cs = int(round(seconds * 100))
    cs = total_cs % 100
    total_s = total_cs // 100
    s = total_s % 60
    m = (total_s // 60) % 60
    h = total_s // 3600
    return f"{h}:{m:02d}:{s:02d}.{cs:02d}"


def load_input_chunk(path: str) -> dict:
    """Load input events from msgpack file.

    The file contains an array of events in tuple format:
    [timestamp_us, [event_type, event_data]]

    For KeyPress/KeyRelease: [timestamp_us, ['KeyPress', [code, name]]]
    """
    with open(path, "rb") as handle:
        data = msgpack.unpackb(handle.read(), raw=False)
    assert isinstance(data, list), "Input file does not contain an array of events."

    normalized_events = []
    for entry in data:
        assert isinstance(entry, list) and len(entry) == 2, "event entry must be [timestamp, event]."
        timestamp_us, event_tuple = entry
        assert isinstance(timestamp_us, int), "event timestamp must be an integer."
        assert isinstance(event_tuple, list) and len(event_tuple) >= 1, "event must be [type, data]."

        event_type = event_tuple[0]
        event_data = event_tuple[1] if len(event_tuple) > 1 else None

        # Convert to dict format for extract_key_events
        if event_type in ("KeyPress", "KeyRelease"):
            # event_data is [code, name]
            code, name = event_data
            event_obj = {"type": event_type, "data": {"code": code, "name": name}}
        elif event_type in ("MousePress", "MouseRelease"):
            # event_data is [button, x, y]
            button, x, y = event_data
            event_obj = {"type": event_type, "data": {"button": button, "x": x, "y": y}}
        elif event_type == "MouseMove":
            # event_data is [delta_x, delta_y]
            delta_x, delta_y = event_data
            event_obj = {"type": event_type, "data": {"delta_x": delta_x, "delta_y": delta_y}}
        elif event_type == "MouseScroll":
            # event_data is [delta_x, delta_y, x, y]
            delta_x, delta_y, x, y = event_data
            event_obj = {"type": event_type, "data": {"delta_x": delta_x, "delta_y": delta_y, "x": x, "y": y}}
        else:
            event_obj = {"type": event_type, "data": event_data}

        normalized_events.append({"timestamp_us": timestamp_us, "event": event_obj})

    end_time_us = normalized_events[-1]["timestamp_us"] if normalized_events else 0
    return {
        "session_id": "unknown",
        "chunk_id": "unknown",
        "start_time_us": 0,
        "end_time_us": end_time_us,
        "events": normalized_events,
        "metadata": {
            "obs_scene": "",
            "pause_count": 0,
            "pause_duration_us": 0,
            "agent_version": "",
            "platform": "",
        },
    }


def extract_key_events(
    chunk: dict,
    allow_unreleased: bool = False,
    strict_keys: bool = False,
) -> list[tuple[float, float, str]]:
    events = []
    assert "events" in chunk, "Input log missing events array."
    assert "start_time_us" in chunk, "Input log missing start_time_us."

    start_time_us = chunk["start_time_us"]
    end_time_us = chunk.get("end_time_us")
    active: dict[str, float] = {}
    last_event_t = 0.0
    for entry in chunk["events"]:
        event = entry.get("event", {})
        event_type = event.get("type")
        if event_type not in ("KeyPress", "KeyRelease"):
            continue
        data = event.get("data", {})
        name = data.get("name")
        assert name, "KeyPress event missing name."
        timestamp_us = entry.get("timestamp_us")
        assert timestamp_us is not None, "Event missing timestamp_us."
        t = (timestamp_us - start_time_us) / 1_000_000.0
        assert t >= 0, "Event timestamp precedes start_time_us."
        if t > last_event_t:
            last_event_t = t
        if event_type == "KeyPress":
            if name in active:
                if strict_keys:
                    assert False, f"KeyPress without release for {name}."
                print(
                    f"Warning: duplicate KeyPress for {name} at {t:.3f}s.",
                    file=sys.stderr,
                )
                continue
            active[name] = t
        else:
            if name not in active:
                if strict_keys:
                    assert False, f"KeyRelease without press for {name}."
                print(
                    f"Warning: KeyRelease without press for {name} at {t:.3f}s.",
                    file=sys.stderr,
                )
                continue
            start = active.pop(name)
            assert t >= start, f"KeyRelease precedes KeyPress for {name}."
            events.append((start, t, normalize_key_name(name)))

    if active:
        if strict_keys and not allow_unreleased:
            assert False, f"Unreleased keys at end of log: {', '.join(sorted(active.keys()))}"
        end_time = last_event_t
        if isinstance(end_time_us, int):
            end_time = max(end_time, (end_time_us - start_time_us) / 1_000_000.0)
        print(
            f"Warning: closing {len(active)} unreleased key(s) at {end_time:.3f}s.",
            file=sys.stderr,
        )
        for name, start in active.items():
            events.append((start, max(start, end_time), normalize_key_name(name)))
    return sorted(events, key=lambda item: item[0])


def probe_video_size(video_path: str) -> tuple[int, int]:
    command = [
        "ffprobe",
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height",
        "-of",
        "json",
        video_path,
    ]
    result = subprocess.run(command, capture_output=True, text=True, check=True)
    payload = json.loads(result.stdout)
    stream = payload["streams"][0]
    return int(stream["width"]), int(stream["height"])


MIN_KEY_DURATION_S = 0.05  # Minimum duration for a key to be visible (50ms)
DEFAULT_SPARK_BUCKET_S = 0.033  # 33ms buckets (~30fps)
DEFAULT_SPARK_SIZE = 80  # px
DEFAULT_SPARK_MIN_LEN = 4  # px
DEFAULT_SPARK_GAIN = 1.0
DEFAULT_CURSOR_BOX_W = 220  # px
DEFAULT_CURSOR_BOX_H = 140  # px
DEFAULT_CURSOR_DOT_SIZE = 6  # px
DEFAULT_CURSOR_SAMPLE_S = 1.0 / 60.0  # 60 Hz
DEFAULT_CURSOR_RECENTER_S = 0.4  # seconds
DEFAULT_CURSOR_GAIN = 1.0


def build_ass(
    key_events: list[tuple[float, float, str]],
    mouse_moves: list[tuple[float, float, float]],
    font: str,
    font_size: int,
    anchor: str,
    margin_l: int,
    margin_r: int,
    margin_v: int,
    width: int,
    height: int,
    spark_size: int,
    spark_bucket_s: float,
    spark_offset_x: int,
    spark_min_len: int,
    spark_gain: float,
    cursor_box_w: int,
    cursor_box_h: int,
    cursor_offset_x: int,
    cursor_offset_y: int,
    cursor_dot_size: int,
    cursor_sample_s: float,
    cursor_recenter_s: float,
    cursor_gain: float,
    cursor_clamp: bool,
) -> str:
    alignment = ANCHOR_ALIGNMENT[anchor]
    lines = [
        "[Script Info]",
        "ScriptType: v4.00+",
        f"PlayResX: {width}",
        f"PlayResY: {height}",
        "ScaledBorderAndShadow: yes",
        "",
        "[V4+ Styles]",
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, "
        "BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, "
        "BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
        (
            "Style: Keylog,{font},{size},&H000000FF,&H000000FF,&H80000000,&H80000000,"
            "0,0,0,0,100,100,0,0,1,2,0,{align},{ml},{mr},{mv},1"
        ).format(
            font=font,
            size=font_size,
            align=alignment,
            ml=margin_l,
            mr=margin_r,
            mv=margin_v,
        ),
        (
            "Style: Sparks,{font},10,&H000000FF,&H000000FF,&H000000FF,&H00000000,"
            "0,0,0,0,100,100,0,0,1,2,0,7,0,0,0,1"
        ).format(font=font),
        "Style: Cursor,Arial,10,&H000000FF,&H000000FF,&H80000000,&H00000000,"
        "0,0,0,0,100,100,0,0,1,2,0,7,0,0,0,1",
        "",
        "[Events]",
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
    ]

    for start_ts, end_ts, name in key_events:
        # Ensure minimum duration so fast keypresses are still visible
        if end_ts - start_ts < MIN_KEY_DURATION_S:
            end_ts = start_ts + MIN_KEY_DURATION_S
        start = format_ass_time(start_ts)
        end = format_ass_time(end_ts)
        text = escape_ass(name)
        lines.append(f"Dialogue: 0,{start},{end},Keylog,,0,0,0,,{text}")

    spark_events = build_spark_events(
        mouse_moves=mouse_moves,
        anchor=anchor,
        margin_l=margin_l,
        margin_r=margin_r,
        margin_v=margin_v,
        width=width,
        height=height,
        font_size=font_size,
        spark_size=spark_size,
        spark_bucket_s=spark_bucket_s,
        spark_offset_x=spark_offset_x,
        spark_min_len=spark_min_len,
        spark_gain=spark_gain,
    )
    for start_ts, end_ts, text in spark_events:
        start = format_ass_time(start_ts)
        end = format_ass_time(end_ts)
        lines.append(f"Dialogue: 0,{start},{end},Sparks,,0,0,0,,{text}")

    cursor_events = build_cursor_events(
        mouse_moves=mouse_moves,
        anchor=anchor,
        margin_l=margin_l,
        margin_r=margin_r,
        margin_v=margin_v,
        width=width,
        height=height,
        font_size=font_size,
        cursor_box_w=cursor_box_w,
        cursor_box_h=cursor_box_h,
        cursor_offset_x=cursor_offset_x,
        cursor_offset_y=cursor_offset_y,
        cursor_dot_size=cursor_dot_size,
        cursor_sample_s=cursor_sample_s,
        cursor_recenter_s=cursor_recenter_s,
        cursor_gain=cursor_gain,
        cursor_clamp=cursor_clamp,
    )
    for start_ts, end_ts, text in cursor_events:
        start = format_ass_time(start_ts)
        end = format_ass_time(end_ts)
        lines.append(f"Dialogue: 0,{start},{end},Cursor,,0,0,0,,{text}")

    return "\n".join(lines) + "\n"


def run_ffmpeg(video_path: str, ass_path: str, output_path: str) -> None:
    command = [
        "ffmpeg",
        "-y",
        "-i",
        video_path,
        "-vf",
        f"subtitles={ass_path}",
        "-c:a",
        "copy",
        output_path,
    ]
    subprocess.run(command, check=True)


def extract_mouse_moves(chunk: dict) -> list[tuple[float, float, float]]:
    events = []
    assert "events" in chunk, "Input log missing events array."
    assert "start_time_us" in chunk, "Input log missing start_time_us."

    start_time_us = chunk["start_time_us"]
    for entry in chunk["events"]:
        event = entry.get("event", {})
        event_type = event.get("type")
        if event_type != "MouseMove":
            continue
        data = event.get("data", {})
        delta_x = data.get("delta_x")
        delta_y = data.get("delta_y")
        if delta_x is None or delta_y is None:
            continue
        timestamp_us = entry.get("timestamp_us")
        assert timestamp_us is not None, "Event missing timestamp_us."
        t = (timestamp_us - start_time_us) / 1_000_000.0
        if t < 0:
            continue
        events.append((t, float(delta_x), float(delta_y)))
    return events


def bucket_mouse_moves(
    mouse_moves: list[tuple[float, float, float]],
    bucket_s: float,
) -> list[tuple[float, float, float, float]]:
    if not mouse_moves:
        return []
    if bucket_s <= 0:
        raise ValueError("bucket_s must be positive.")

    buckets = []
    current_idx = int(mouse_moves[0][0] / bucket_s)
    start = current_idx * bucket_s
    end = start + bucket_s
    sum_dx = 0.0
    sum_dy = 0.0

    for t, dx, dy in mouse_moves:
        idx = int(t / bucket_s)
        if idx != current_idx:
            if sum_dx != 0.0 or sum_dy != 0.0:
                buckets.append((start, end, sum_dx, sum_dy))
            current_idx = idx
            start = current_idx * bucket_s
            end = start + bucket_s
            sum_dx = 0.0
            sum_dy = 0.0
        sum_dx += dx
        sum_dy += dy

    if sum_dx != 0.0 or sum_dy != 0.0:
        buckets.append((start, end, sum_dx, sum_dy))
    return buckets


def compute_anchor_pos(
    anchor: str,
    margin_l: int,
    margin_r: int,
    margin_v: int,
    width: int,
    height: int,
    font_size: int,
) -> tuple[float, float]:
    if anchor in ("bottom-left", "bottom", "bottom-right"):
        y = height - margin_v - (font_size * 0.5)
    elif anchor in ("top-left", "top", "top-right"):
        y = margin_v + (font_size * 0.5)
    else:
        y = height * 0.5

    if anchor in ("bottom-left", "left", "top-left"):
        x = margin_l
    elif anchor in ("bottom-right", "right", "top-right"):
        x = width - margin_r
    else:
        x = width * 0.5
    return x, y


def compute_spark_hub_pos(
    anchor: str,
    base_x: float,
    base_y: float,
    spark_offset_x: int,
) -> tuple[float, float]:
    if anchor in ("bottom-right", "right", "top-right"):
        return base_x - spark_offset_x, base_y
    return base_x + spark_offset_x, base_y


def compute_cursor_hub_pos(
    anchor: str,
    base_x: float,
    base_y: float,
    cursor_offset_x: int,
    cursor_offset_y: int,
) -> tuple[float, float]:
    if anchor in ("bottom-right", "right", "top-right"):
        return base_x - cursor_offset_x, base_y + cursor_offset_y
    return base_x + cursor_offset_x, base_y + cursor_offset_y


def format_draw_value(value: float) -> str:
    text = f"{value:.1f}"
    return text.rstrip("0").rstrip(".")


def build_spark_draw(
    delta_x: float,
    delta_y: float,
    spark_size: int,
    spark_min_len: int,
    spark_gain: float,
) -> str | None:
    magnitude = math.hypot(delta_x, delta_y)
    if magnitude <= 0.0:
        return None

    length = min(spark_size, magnitude * spark_gain)
    if length < spark_min_len:
        length = spark_min_len

    angle = math.atan2(delta_y, delta_x)
    side_angle = 0.35  # ~20 degrees
    side_len = length * 0.6

    main_dx = length * math.cos(angle)
    main_dy = length * math.sin(angle)
    left_dx = side_len * math.cos(angle + side_angle)
    left_dy = side_len * math.sin(angle + side_angle)
    right_dx = side_len * math.cos(angle - side_angle)
    right_dy = side_len * math.sin(angle - side_angle)

    def seg(dx: float, dy: float) -> str:
        return f"m 0 0 l {format_draw_value(dx)} {format_draw_value(dy)}"

    return " ".join([seg(main_dx, main_dy), seg(left_dx, left_dy), seg(right_dx, right_dy)])


def build_spark_events(
    mouse_moves: list[tuple[float, float, float]],
    anchor: str,
    margin_l: int,
    margin_r: int,
    margin_v: int,
    width: int,
    height: int,
    font_size: int,
    spark_size: int,
    spark_bucket_s: float,
    spark_offset_x: int,
    spark_min_len: int,
    spark_gain: float,
) -> list[tuple[float, float, str]]:
    buckets = bucket_mouse_moves(mouse_moves, spark_bucket_s)
    if not buckets:
        return []

    base_x, base_y = compute_anchor_pos(
        anchor=anchor,
        margin_l=margin_l,
        margin_r=margin_r,
        margin_v=margin_v,
        width=width,
        height=height,
        font_size=font_size,
    )
    spark_x, spark_y = compute_spark_hub_pos(anchor, base_x, base_y, spark_offset_x)

    events = []
    for start, end, dx, dy in buckets:
        draw = build_spark_draw(dx, dy, spark_size, spark_min_len, spark_gain)
        if not draw:
            continue
        text = (
            r"{\pos("
            + format_draw_value(spark_x)
            + ","
            + format_draw_value(spark_y)
            + r")\an7\p1}"
            + draw
            + r"{\p0}"
        )
        events.append((start, end, text))
    return events


def build_cursor_draw(dot_size: int) -> str:
    half = max(1.0, dot_size * 0.5)
    h = format_draw_value(half)
    nh = format_draw_value(-half)
    return f"m {nh} {nh} l {h} {nh} l {h} {h} l {nh} {h}"


def build_cursor_events(
    mouse_moves: list[tuple[float, float, float]],
    anchor: str,
    margin_l: int,
    margin_r: int,
    margin_v: int,
    width: int,
    height: int,
    font_size: int,
    cursor_box_w: int,
    cursor_box_h: int,
    cursor_offset_x: int,
    cursor_offset_y: int,
    cursor_dot_size: int,
    cursor_sample_s: float,
    cursor_recenter_s: float,
    cursor_gain: float,
    cursor_clamp: bool,
) -> list[tuple[float, float, str]]:
    if not mouse_moves:
        return []
    if cursor_sample_s <= 0:
        raise ValueError("cursor_sample_s must be positive.")

    base_x, base_y = compute_anchor_pos(
        anchor=anchor,
        margin_l=margin_l,
        margin_r=margin_r,
        margin_v=margin_v,
        width=width,
        height=height,
        font_size=font_size,
    )
    hub_x, hub_y = compute_cursor_hub_pos(
        anchor=anchor,
        base_x=base_x,
        base_y=base_y,
        cursor_offset_x=cursor_offset_x,
        cursor_offset_y=cursor_offset_y,
    )

    half_w = max(1.0, cursor_box_w * 0.5)
    half_h = max(1.0, cursor_box_h * 0.5)

    events = []
    draw = build_cursor_draw(cursor_dot_size)

    mouse_moves = sorted(mouse_moves, key=lambda item: item[0])
    last_t = mouse_moves[-1][0]
    tail = max(0.1, cursor_recenter_s * 2.0)
    end_time = last_t + tail

    pos_x = 0.0
    pos_y = 0.0
    idx = 0
    t = 0.0
    prev_t = 0.0

    while t <= end_time + 1e-6:
        dt = t - prev_t
        if cursor_recenter_s > 0 and dt > 0:
            step_decay = math.exp(-dt / cursor_recenter_s)
            pos_x *= step_decay
            pos_y *= step_decay

        while idx < len(mouse_moves) and mouse_moves[idx][0] <= t:
            _, dx, dy = mouse_moves[idx]
            pos_x += dx * cursor_gain
            pos_y += dy * cursor_gain
            idx += 1

        if cursor_clamp:
            if pos_x > half_w:
                pos_x = half_w
            elif pos_x < -half_w:
                pos_x = -half_w
            if pos_y > half_h:
                pos_y = half_h
            elif pos_y < -half_h:
                pos_y = -half_h

        x = hub_x + pos_x
        y = hub_y + pos_y
        text = (
            r"{\pos("
            + format_draw_value(x)
            + ","
            + format_draw_value(y)
            + r")\an7\p1}"
            + draw
            + r"{\p0}"
        )
        events.append((t, t + cursor_sample_s, text))

        prev_t = t
        t += cursor_sample_s

    return events


def main() -> int:
    parser = argparse.ArgumentParser(description="Overlay keylogs on screen captures.")
    parser.add_argument("--video", help="Path to screen capture video (mp4/mkv).")
    parser.add_argument("--input", required=True, help="Path to input msgpack log.")
    output_group = parser.add_mutually_exclusive_group(required=True)
    output_group.add_argument("--output", help="Output video path (burned-in overlays).")
    output_group.add_argument("--ass-out", help="Write ASS subtitles to this path.")
    parser.add_argument("--font", default="Helvetica", help="Font name for overlay.")
    parser.add_argument("--font-size", type=int, default=36, help="Font size for overlay.")
    parser.add_argument(
        "--anchor",
        default="bottom-left",
        choices=sorted(ANCHOR_ALIGNMENT.keys()),
        help="Overlay anchor position.",
    )
    parser.add_argument("--margin-l", type=int, default=28, help="Left margin.")
    parser.add_argument("--margin-r", type=int, default=28, help="Right margin.")
    parser.add_argument("--margin-v", type=int, default=32, help="Vertical margin.")
    parser.add_argument(
        "--allow-unreleased-keys",
        action="store_true",
        help="Close any unreleased keys at the end of the log instead of failing.",
    )
    parser.add_argument(
        "--strict-keys",
        action="store_true",
        help="Fail fast on duplicate presses, unmatched releases, or unreleased keys.",
    )
    parser.add_argument("--width", type=int, help="ASS width (required without --video).")
    parser.add_argument("--height", type=int, help="ASS height (required without --video).")
    parser.add_argument("--spark-size", type=int, default=DEFAULT_SPARK_SIZE, help="Max spark length in pixels.")
    parser.add_argument(
        "--spark-bucket-ms",
        type=int,
        default=int(DEFAULT_SPARK_BUCKET_S * 1000),
        help="Bucket size in ms for mouse velocity sampling.",
    )
    parser.add_argument(
        "--spark-offset-x",
        type=int,
        help="Horizontal offset from key text anchor to spark hub (defaults to max(spark_size, font_size * 4)).",
    )
    parser.add_argument(
        "--spark-min-len",
        type=int,
        default=DEFAULT_SPARK_MIN_LEN,
        help="Minimum spark length in pixels.",
    )
    parser.add_argument(
        "--spark-gain",
        type=float,
        default=DEFAULT_SPARK_GAIN,
        help="Multiplier applied to mouse deltas before clamping to spark size.",
    )
    parser.add_argument(
        "--cursor-box-w",
        type=int,
        default=DEFAULT_CURSOR_BOX_W,
        help="Width of the cursor HUD box in pixels.",
    )
    parser.add_argument(
        "--cursor-box-h",
        type=int,
        default=DEFAULT_CURSOR_BOX_H,
        help="Height of the cursor HUD box in pixels.",
    )
    parser.add_argument(
        "--cursor-offset-x",
        type=int,
        help="Horizontal offset from key text anchor to cursor center.",
    )
    parser.add_argument(
        "--cursor-offset-y",
        type=int,
        default=0,
        help="Vertical offset from key text anchor to cursor center.",
    )
    parser.add_argument(
        "--cursor-dot-size",
        type=int,
        default=DEFAULT_CURSOR_DOT_SIZE,
        help="Cursor dot size in pixels.",
    )
    parser.add_argument(
        "--cursor-sample-ms",
        type=int,
        default=int(DEFAULT_CURSOR_SAMPLE_S * 1000),
        help="Sampling interval in ms for cursor rendering.",
    )
    parser.add_argument(
        "--cursor-recenter-ms",
        type=int,
        default=int(DEFAULT_CURSOR_RECENTER_S * 1000),
        help="Time constant in ms for smooth recentering.",
    )
    parser.add_argument(
        "--cursor-gain",
        type=float,
        default=DEFAULT_CURSOR_GAIN,
        help="Multiplier applied to mouse deltas for cursor movement.",
    )
    parser.add_argument(
        "--cursor-no-clamp",
        action="store_true",
        help="Disable clamping the cursor within the cursor box.",
    )

    args = parser.parse_args()

    chunk = load_input_chunk(args.input)
    key_events = extract_key_events(
        chunk,
        allow_unreleased=args.allow_unreleased_keys,
        strict_keys=args.strict_keys,
    )
    mouse_moves = extract_mouse_moves(chunk)

    if not key_events:
        parser.error("No KeyPress events found in input log.")

    if args.video:
        width, height = probe_video_size(args.video)
    else:
        if args.width is None or args.height is None:
            parser.error("--width and --height are required when --video is not provided.")
        width, height = args.width, args.height

    ass_text = build_ass(
        key_events=key_events,
        mouse_moves=mouse_moves,
        font=args.font,
        font_size=args.font_size,
        anchor=args.anchor,
        margin_l=args.margin_l,
        margin_r=args.margin_r,
        margin_v=args.margin_v,
        width=width,
        height=height,
        spark_size=args.spark_size,
        spark_bucket_s=max(0.001, args.spark_bucket_ms / 1000.0),
        spark_offset_x=(
            args.spark_offset_x
            if args.spark_offset_x is not None
            else max(args.spark_size, int(args.font_size * 4))
        ),
        spark_min_len=args.spark_min_len,
        spark_gain=args.spark_gain,
        cursor_box_w=args.cursor_box_w,
        cursor_box_h=args.cursor_box_h,
        cursor_offset_x=(
            args.cursor_offset_x
            if args.cursor_offset_x is not None
            else max(args.cursor_box_w // 2, int(args.font_size * 4))
        ),
        cursor_offset_y=args.cursor_offset_y,
        cursor_dot_size=args.cursor_dot_size,
        cursor_sample_s=max(0.001, args.cursor_sample_ms / 1000.0),
        cursor_recenter_s=max(0.001, args.cursor_recenter_ms / 1000.0),
        cursor_gain=args.cursor_gain,
        cursor_clamp=not args.cursor_no_clamp,
    )

    ass_path = args.ass_out
    temp_handle = None
    if not ass_path:
        temp_handle = tempfile.NamedTemporaryFile(delete=False, suffix=".ass")
        ass_path = temp_handle.name
        temp_handle.close()

    with open(ass_path, "w", encoding="utf-8") as handle:
        handle.write(ass_text)

    if args.output:
        if not args.video:
            print("--video is required when using --output.", file=sys.stderr)
            return 1
        run_ffmpeg(args.video, ass_path, args.output)

    if temp_handle and os.path.exists(ass_path):
        os.unlink(ass_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
