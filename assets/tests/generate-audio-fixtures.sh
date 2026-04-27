#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${OUT_DIR:-$SCRIPT_DIR/audio-fixtures}"
DURATION="${DURATION:-0.15}"
RATE="${RATE:-8000}"
CHANNEL_LAYOUT="${CHANNEL_LAYOUT:-mono}"

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: missing required command: $1" >&2
        exit 1
    fi
}

require_command ffmpeg
require_command ffprobe
require_command python3
require_command metaflac
require_command vorbiscomment

python3 - <<'PY'
import importlib.util
import sys
if importlib.util.find_spec("mutagen") is None:
    print("error: missing Python package: mutagen", file=sys.stderr)
    sys.exit(1)
PY

mkdir -p "$OUT_DIR"
rm -f \
    "$OUT_DIR"/fixture.{mp3,flac,ogg,m4a,wav,aiff,opus,aac} \
    "$OUT_DIR"/cover.jpg

TITLE="Test Track"
ARTIST="Test Artist"
ALBUM_ARTIST="Test Album Artist"
ALBUM="Test Album"
GENRE="Test Genre"
DATE="1995-06-24"
TRACK="2"
TRACK_TOTAL="9"
DISC="1"
DISC_TOTAL="3"
ISRC="QZHB12400001"
MBID_ALBUM="12345678-1234-4234-9234-123456789abc"
REPLAYGAIN_TRACK_GAIN="-3.21 dB"
REPLAYGAIN_TRACK_PEAK="0.987654"
REPLAYGAIN_ALBUM_GAIN="-4.56 dB"
REPLAYGAIN_ALBUM_PEAK="0.876543"
LYRICS="[00:00.00] Test lyrics"

COMMON_METADATA=(
    -metadata "title=$TITLE"
    -metadata "artist=$ARTIST"
    -metadata "album_artist=$ALBUM_ARTIST"
    -metadata "album=$ALBUM"
    -metadata "genre=$GENRE"
    -metadata "date=$DATE"
    -metadata "track=$TRACK/$TRACK_TOTAL"
    -metadata "disc=$DISC/$DISC_TOTAL"
    -metadata "isrc=$ISRC"
    -metadata "MusicBrainz Album Id=$MBID_ALBUM"
    -metadata "replaygain_track_gain=$REPLAYGAIN_TRACK_GAIN"
    -metadata "replaygain_track_peak=$REPLAYGAIN_TRACK_PEAK"
    -metadata "replaygain_album_gain=$REPLAYGAIN_ALBUM_GAIN"
    -metadata "replaygain_album_peak=$REPLAYGAIN_ALBUM_PEAK"
    -metadata "lyrics=$LYRICS"
)

make_silence() {
    local output="$1"
    shift
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "anullsrc=r=$RATE:cl=$CHANNEL_LAYOUT" \
        -t "$DURATION" \
        "${COMMON_METADATA[@]}" \
        "$@" \
        "$output"
}

ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "color=c=#336699:s=32x32:d=0.1" \
    -frames:v 1 \
    "$OUT_DIR/cover.jpg"

make_silence "$OUT_DIR/fixture.mp3" -c:a libmp3lame -q:a 9 -id3v2_version 3
make_silence "$OUT_DIR/fixture.flac" -c:a flac -compression_level 12
make_silence "$OUT_DIR/fixture.ogg" -c:a libvorbis -q:a -1
make_silence "$OUT_DIR/fixture.m4a" -c:a aac -b:a 24k -movflags +faststart
make_silence "$OUT_DIR/fixture.wav" -c:a pcm_s16le
make_silence "$OUT_DIR/fixture.aiff" -c:a pcm_s16be
make_silence "$OUT_DIR/fixture.opus" -c:a libopus -b:a 12k -vbr off
make_silence "$OUT_DIR/fixture.aac" -c:a aac -b:a 24k -f adts -write_id3v2 1

metaflac --remove-all-tags "$OUT_DIR/fixture.flac"
metaflac \
    --set-tag="TITLE=$TITLE" \
    --set-tag="ARTIST=$ARTIST" \
    --set-tag="ALBUMARTIST=$ALBUM_ARTIST" \
    --set-tag="ALBUM=$ALBUM" \
    --set-tag="GENRE=$GENRE" \
    --set-tag="DATE=$DATE" \
    --set-tag="TRACKNUMBER=$TRACK" \
    --set-tag="TRACKTOTAL=$TRACK_TOTAL" \
    --set-tag="DISCNUMBER=$DISC" \
    --set-tag="DISCTOTAL=$DISC_TOTAL" \
    --set-tag="ISRC=$ISRC" \
    --set-tag="MUSICBRAINZ_ALBUMID=$MBID_ALBUM" \
    --set-tag="REPLAYGAIN_TRACK_GAIN=$REPLAYGAIN_TRACK_GAIN" \
    --set-tag="REPLAYGAIN_TRACK_PEAK=$REPLAYGAIN_TRACK_PEAK" \
    --set-tag="REPLAYGAIN_ALBUM_GAIN=$REPLAYGAIN_ALBUM_GAIN" \
    --set-tag="REPLAYGAIN_ALBUM_PEAK=$REPLAYGAIN_ALBUM_PEAK" \
    --set-tag="LYRICS=$LYRICS" \
    --import-picture-from="$OUT_DIR/cover.jpg" \
    "$OUT_DIR/fixture.flac"

vorbiscomment -w \
    -t "TITLE=$TITLE" \
    -t "ARTIST=$ARTIST" \
    -t "ALBUMARTIST=$ALBUM_ARTIST" \
    -t "ALBUM=$ALBUM" \
    -t "GENRE=$GENRE" \
    -t "DATE=$DATE" \
    -t "TRACKNUMBER=$TRACK" \
    -t "TRACKTOTAL=$TRACK_TOTAL" \
    -t "DISCNUMBER=$DISC" \
    -t "DISCTOTAL=$DISC_TOTAL" \
    -t "ISRC=$ISRC" \
    -t "MUSICBRAINZ_ALBUMID=$MBID_ALBUM" \
    -t "REPLAYGAIN_TRACK_GAIN=$REPLAYGAIN_TRACK_GAIN" \
    -t "REPLAYGAIN_TRACK_PEAK=$REPLAYGAIN_TRACK_PEAK" \
    -t "REPLAYGAIN_ALBUM_GAIN=$REPLAYGAIN_ALBUM_GAIN" \
    -t "REPLAYGAIN_ALBUM_PEAK=$REPLAYGAIN_ALBUM_PEAK" \
    -t "LYRICS=$LYRICS" \
    "$OUT_DIR/fixture.ogg"

export OUT_DIR TITLE ARTIST ALBUM_ARTIST ALBUM GENRE DATE TRACK TRACK_TOTAL DISC DISC_TOTAL ISRC MBID_ALBUM \
    REPLAYGAIN_TRACK_GAIN REPLAYGAIN_TRACK_PEAK REPLAYGAIN_ALBUM_GAIN REPLAYGAIN_ALBUM_PEAK LYRICS

python3 - <<'PY'
import os
from pathlib import Path

from mutagen.flac import Picture
from mutagen.aiff import AIFF
from mutagen.id3 import APIC, COMM, ID3, TALB, TCON, TCMP, TDRC, TIT2, TPE1, TPE2, TPOS, TRCK, TSRC, TXXX, USLT
from mutagen.mp4 import MP4, MP4Cover, MP4FreeForm
from mutagen.wave import WAVE

out_dir = Path(os.environ["OUT_DIR"])
cover = (out_dir / "cover.jpg").read_bytes()

values = {
    "title": os.environ["TITLE"],
    "artist": os.environ["ARTIST"],
    "album_artist": os.environ["ALBUM_ARTIST"],
    "album": os.environ["ALBUM"],
    "genre": os.environ["GENRE"],
    "date": os.environ["DATE"],
    "track": os.environ["TRACK"],
    "track_total": os.environ["TRACK_TOTAL"],
    "disc": os.environ["DISC"],
    "disc_total": os.environ["DISC_TOTAL"],
    "isrc": os.environ["ISRC"],
    "mbid_album": os.environ["MBID_ALBUM"],
    "rg_track_gain": os.environ["REPLAYGAIN_TRACK_GAIN"],
    "rg_track_peak": os.environ["REPLAYGAIN_TRACK_PEAK"],
    "rg_album_gain": os.environ["REPLAYGAIN_ALBUM_GAIN"],
    "rg_album_peak": os.environ["REPLAYGAIN_ALBUM_PEAK"],
    "lyrics": os.environ["LYRICS"],
}

def add_id3_frames(tags) -> None:
    tags.add(TIT2(encoding=3, text=[values["title"]]))
    tags.add(TPE1(encoding=3, text=[values["artist"]]))
    tags.add(TPE2(encoding=3, text=[values["album_artist"]]))
    tags.add(TALB(encoding=3, text=[values["album"]]))
    tags.add(TCON(encoding=3, text=[values["genre"]]))
    tags.add(TDRC(encoding=3, text=[values["date"]]))
    tags.add(TRCK(encoding=3, text=[f'{values["track"]}/{values["track_total"]}']))
    tags.add(TPOS(encoding=3, text=[f'{values["disc"]}/{values["disc_total"]}']))
    tags.add(TCMP(encoding=3, text=["1"]))
    tags.add(TSRC(encoding=3, text=[values["isrc"]]))
    tags.add(TXXX(encoding=3, desc="MusicBrainz Album Id", text=[values["mbid_album"]]))
    tags.add(TXXX(encoding=3, desc="REPLAYGAIN_TRACK_GAIN", text=[values["rg_track_gain"]]))
    tags.add(TXXX(encoding=3, desc="REPLAYGAIN_TRACK_PEAK", text=[values["rg_track_peak"]]))
    tags.add(TXXX(encoding=3, desc="REPLAYGAIN_ALBUM_GAIN", text=[values["rg_album_gain"]]))
    tags.add(TXXX(encoding=3, desc="REPLAYGAIN_ALBUM_PEAK", text=[values["rg_album_peak"]]))
    tags.add(USLT(encoding=3, lang="eng", desc="", text=values["lyrics"]))
    tags.add(COMM(encoding=3, lang="eng", desc="", text=["Test audio file"]))
    tags.add(APIC(encoding=3, mime="image/jpeg", type=3, desc="Cover", data=cover))

def tag_id3(path: Path) -> None:
    tags = ID3()
    add_id3_frames(tags)
    tags.save(path, v2_version=3)

def tag_id3_container(path: Path, cls) -> None:
    audio = cls(path)
    if audio.tags is None:
        audio.add_tags()
    else:
        audio.tags.clear()
    add_id3_frames(audio.tags)
    audio.save()

for name in ["fixture.mp3", "fixture.aac"]:
    tag_id3(out_dir / name)

for name, cls in [("fixture.wav", WAVE), ("fixture.aiff", AIFF)]:
    tag_id3_container(out_dir / name, cls)

mp4 = MP4(out_dir / "fixture.m4a")
mp4.clear()
# Mutagen uses \xa9 for iTunes-style MP4 atoms whose names start with ©, such as ©nam
# for title and ©day for release date.
mp4["\xa9nam"] = [values["title"]]
mp4["\xa9ART"] = [values["artist"]]
mp4["aART"] = [values["album_artist"]]
mp4["\xa9alb"] = [values["album"]]
mp4["\xa9gen"] = [values["genre"]]
mp4["\xa9day"] = [values["date"]]
mp4["trkn"] = [(int(values["track"]), int(values["track_total"]))]
mp4["disk"] = [(int(values["disc"]), int(values["disc_total"]))]
mp4["cpil"] = [True]
mp4["----:com.apple.iTunes:ISRC"] = [MP4FreeForm(values["isrc"].encode())]
mp4["----:com.apple.iTunes:MusicBrainz Album Id"] = [MP4FreeForm(values["mbid_album"].encode())]
mp4["----:com.apple.iTunes:REPLAYGAIN_TRACK_GAIN"] = [MP4FreeForm(values["rg_track_gain"].encode())]
mp4["----:com.apple.iTunes:REPLAYGAIN_TRACK_PEAK"] = [MP4FreeForm(values["rg_track_peak"].encode())]
mp4["----:com.apple.iTunes:REPLAYGAIN_ALBUM_GAIN"] = [MP4FreeForm(values["rg_album_gain"].encode())]
mp4["----:com.apple.iTunes:REPLAYGAIN_ALBUM_PEAK"] = [MP4FreeForm(values["rg_album_peak"].encode())]
mp4["\xa9lyr"] = [values["lyrics"]]
mp4["covr"] = [MP4Cover(cover, imageformat=MP4Cover.FORMAT_JPEG)]
mp4.save()

picture = Picture()
picture.type = 3
picture.mime = "image/jpeg"
picture.desc = "Cover"
picture.data = cover
# Vorbis and Opus use the standard METADATA_BLOCK_PICTURE value. Mutagen stores this as
# base64 text; build it directly to avoid relying on format-specific picture helpers.
import base64
picture_text = base64.b64encode(picture.write()).decode("ascii")

import mutagen

audio = mutagen.File(out_dir / "fixture.ogg")
audio["METADATA_BLOCK_PICTURE"] = [picture_text]
audio.save()

opus = mutagen.File(out_dir / "fixture.opus")
opus.clear()
opus["TITLE"] = [values["title"]]
opus["ARTIST"] = [values["artist"]]
opus["ALBUMARTIST"] = [values["album_artist"]]
opus["ALBUM"] = [values["album"]]
opus["GENRE"] = [values["genre"]]
opus["DATE"] = [values["date"]]
opus["TRACKNUMBER"] = [values["track"]]
opus["TRACKTOTAL"] = [values["track_total"]]
opus["DISCNUMBER"] = [values["disc"]]
opus["DISCTOTAL"] = [values["disc_total"]]
opus["ISRC"] = [values["isrc"]]
opus["MUSICBRAINZ_ALBUMID"] = [values["mbid_album"]]
opus["REPLAYGAIN_TRACK_GAIN"] = [values["rg_track_gain"]]
opus["REPLAYGAIN_TRACK_PEAK"] = [values["rg_track_peak"]]
opus["REPLAYGAIN_ALBUM_GAIN"] = [values["rg_album_gain"]]
opus["REPLAYGAIN_ALBUM_PEAK"] = [values["rg_album_peak"]]
opus["LYRICS"] = [values["lyrics"]]
opus["METADATA_BLOCK_PICTURE"] = [picture_text]
opus.save()
PY

for file in "$OUT_DIR"/fixture.*; do
    ffprobe -hide_banner -loglevel error -show_format -show_streams "$file" >/dev/null
    printf 'created %s (%s bytes)\n' "${file#$SCRIPT_DIR/}" "$(wc -c <"$file")"
done
