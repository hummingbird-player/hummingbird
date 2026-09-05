#!/usr/bin/env python3
"""Generate synthetic music/config for disposable Subsonic acceptance servers.

Requires ffmpeg. Does not download software or launch servers. The destination
must be new: this tool never changes an existing server or music collection.
"""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    root = args.directory.resolve()
    root.mkdir(mode=0o700, parents=True, exist_ok=False)
    for name in ["navidrome", "gonic/cache", "gonic/podcasts", "gonic/playlists"]:
        (root / name).mkdir(parents=True)
    for album, title, frequency in [
        ("Alpha", "HB Fixture One", 440),
        ("Alpha", "HB Fixture Two", 660),
        ("Beta", "HB Fixture Three", 880),
    ]:
        folder = root / "music" / "HB Fixture Artist" / album
        folder.mkdir(parents=True, exist_ok=True)
        command = ["ffmpeg", "-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i",
                   f"sine=frequency={frequency}:duration=90:sample_rate=44100",
                   "-ac", "2", "-c:a", "flac"]
        for tag in [f"title={title}", "artist=HB Fixture Artist",
                    "album_artist=HB Fixture Artist", f"album={album}",
                    "REPLAYGAIN_TRACK_GAIN=-6.00 dB",
                    "lyrics=HB fixture lyric one\nHB fixture lyric two"]:
            command += ["-metadata", tag]
        subprocess.run(command + [str(folder / (title + ".flac"))], check=True)
        subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-y", "-f", "lavfi",
                        "-i", "color=c=red:s=64x64", "-frames:v", "1",
                        str(folder / "cover.png")], check=True)
        (folder / (title + ".lrc")).write_text(
            "[00:00.00]HB fixture lyric one\n[00:10.00]HB fixture lyric two\n")
    subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i",
                    "sine=frequency=220:duration=5:sample_rate=48000", "-c:a", "pcm_s16le",
                    str(root / "music" / "HB Fixture Loose.wav")], check=True)
    # JSON quoting also produces valid TOML basic strings for these filesystem paths.
    (root / "navidrome.toml").write_text(
        'Address = "127.0.0.1"\nPort = 14533\nBaseUrl = "/navidrome"\n'
        f'MusicFolder = {json.dumps(str(root / "music"))}\n'
        f'DataFolder = {json.dumps(str(root / "navidrome"))}\n'
        'EnableInsightsCollector = false\nEnableExternalServices = false\n'
        'ScanSchedule = "0"\nLogLevel = "warn"\n')
    endpoints = root / "endpoints.json"
    endpoints.write_text(json.dumps([
        dict(name="navidrome", endpoint="http://127.0.0.1:14533/navidrome",
             username="hb-fixture", password="hb-fixture-test-only"),
        dict(name="gonic", endpoint="http://127.0.0.1:14747",
             username="admin", password="admin"),
    ], indent=2))
    endpoints.chmod(0o600)
    print(f"Fixture created at {root}")


if __name__ == "__main__":
    main()
