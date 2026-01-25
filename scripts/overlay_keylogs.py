#!/usr/bin/env python3
import argparse
import json
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
    with open(path, "rb") as handle:
        data = msgpack.unpackb(handle.read(), raw=False)
    if isinstance(data, dict):
        return data
    assert isinstance(data, list), "Input file does not contain a map or array."
    assert len(data) == 6, "Input array must have 6 elements."
    session_id, chunk_id, start_time_us, end_time_us, events, metadata = data
    assert isinstance(start_time_us, int), "start_time_us must be an integer."
    assert isinstance(events, list), "events must be an array."
    assert isinstance(metadata, list), "metadata must be an array."
    assert len(metadata) == 5, "metadata array must have 5 elements."

    normalized_events = []
    for entry in events:
        assert isinstance(entry, list) and len(entry) == 2, "event entry must be [timestamp, payload]."
        timestamp_us, payload = entry
        assert isinstance(timestamp_us, int), "event timestamp must be an integer."
        assert isinstance(payload, list) and len(payload) == 2, "event payload must be [type, data]."
        event_type, event_data = payload
        assert isinstance(event_type, str), "event type must be a string."

        event_obj: dict
        if event_type in ("KeyPress", "KeyRelease"):
            assert isinstance(event_data, list) and len(event_data) == 2, "key event data must be [code, name]."
            code, name = event_data
            assert isinstance(name, str), "key name must be a string."
            event_obj = {"type": event_type, "data": {"code": code, "name": name}}
        elif event_type in ("MousePress", "MouseRelease"):
            assert isinstance(event_data, list) and len(event_data) == 3, "mouse button data must be [button, x, y]."
            button, x, y = event_data
            event_obj = {"type": event_type, "data": {"button": button, "x": x, "y": y}}
        elif event_type == "MouseMove":
            assert isinstance(event_data, list) and len(event_data) == 2, "mouse move data must be [delta_x, delta_y]."
            delta_x, delta_y = event_data
            event_obj = {"type": event_type, "data": {"delta_x": delta_x, "delta_y": delta_y}}
        elif event_type == "MouseScroll":
            assert isinstance(event_data, list) and len(event_data) == 4, "mouse scroll data must be [dx, dy, x, y]."
            delta_x, delta_y, x, y = event_data
            event_obj = {
                "type": event_type,
                "data": {"delta_x": delta_x, "delta_y": delta_y, "x": x, "y": y},
            }
        else:
            assert False, f"Unknown event type: {event_type}"

        normalized_events.append({"timestamp_us": timestamp_us, "event": event_obj})

    obs_scene, pause_count, pause_duration_us, agent_version, platform = metadata
    return {
        "session_id": session_id,
        "chunk_id": chunk_id,
        "start_time_us": start_time_us,
        "end_time_us": end_time_us,
        "events": normalized_events,
        "metadata": {
            "obs_scene": obs_scene,
            "pause_count": pause_count,
            "pause_duration_us": pause_duration_us,
            "agent_version": agent_version,
            "platform": platform,
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


def build_ass(
    key_events: list[tuple[float, float, str]],
    font: str,
    font_size: int,
    anchor: str,
    margin_l: int,
    margin_r: int,
    margin_v: int,
    width: int,
    height: int,
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
            "Style: Keylog,{font},{size},&H00FFFFFF,&H000000FF,&H80000000,&H80000000,"
            "0,0,0,0,100,100,0,0,1,2,0,{align},{ml},{mr},{mv},1"
        ).format(
            font=font,
            size=font_size,
            align=alignment,
            ml=margin_l,
            mr=margin_r,
            mv=margin_v,
        ),
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

    args = parser.parse_args()

    chunk = load_input_chunk(args.input)
    key_events = extract_key_events(
        chunk,
        allow_unreleased=args.allow_unreleased_keys,
        strict_keys=args.strict_keys,
    )

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
        font=args.font,
        font_size=args.font_size,
        anchor=args.anchor,
        margin_l=args.margin_l,
        margin_r=args.margin_r,
        margin_v=args.margin_v,
        width=width,
        height=height,
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
