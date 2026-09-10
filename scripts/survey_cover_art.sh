#!/bin/sh
# Surveys a real music library for which cover-art convention each album
# folder actually uses, before Phase 13 (external cover-art file support)
# decides a lookup order. Run this on the NAS/box that holds the real
# library, not in this repo's checkout.
#
# POSIX sh, not bash-specific: no arrays, no [[ ]], no process substitution
# - this needs to run on whatever shell a NAS (Synology/QNAP/TrueNAS/etc.)
# actually ships, which is often BusyBox or a minimal ash, not bash.
#
# v3: v2 only looked one level up for a loose image file, not for an art
# subdirectory. Many real albums here put tracks in a disc subfolder
# (Album/CD1/track.flac) with art in a sibling subfolder of Album, not a
# loose file (Album/Artwork/cover.jpg, Album/Scans covers/*.jpg). v2
# never checked that shape, which likely explains a good chunk of its
# "none" count. v3 checks both directory levels the same way, and widens
# the subdirectory match from a fixed name list to anything containing
# "art", "cover", or "scan", case-insensitive, since real folder names
# vary too much to enumerate ("Artwork", "Scans covers", "-scans").
#
# Usage: ./survey_cover_art.sh /path/to/music

MUSIC_DIR="${1:?usage: survey_cover_art.sh /path/to/music}"

has_image_named() {
  # $1: directory to look in. $2: basename (without extension).
  find "$1" -maxdepth 1 -type f \
    \( -iname "$2.jpg" -o -iname "$2.jpeg" -o -iname "$2.png" \) 2>/dev/null \
    | grep -q .
}

has_any_image() {
  # $1: directory to look in.
  find "$1" -maxdepth 1 -type f \
    \( -iname '*.jpg' -o -iname '*.jpeg' -o -iname '*.png' \) 2>/dev/null \
    | grep -q .
}

has_art_subdir() {
  # $1: directory to look in. Echoes the first matching subdir's name.
  # -mindepth 1 excludes $1 itself: without it, a music folder whose own
  # name happens to contain "art"/"cover"/"scan" (e.g. a NAS share named
  # "covertest") would match against itself.
  find "$1" -mindepth 1 -maxdepth 1 -type d \
    \( -iname '*art*' -o -iname '*cover*' -o -iname '*scan*' \) 2>/dev/null \
    | head -n1
}

# Runs every check against one directory. Echoes a label, or "none".
classify_dir() {
  dir="$1"
  for base in cover folder front albumart album art thumb; do
    if has_image_named "$dir" "$base"; then
      echo "$base.*"
      return
    fi
  done
  if has_any_image "$dir"; then
    echo "other-image-file"
    return
  fi
  subdir=$(has_art_subdir "$dir")
  if [ -n "$subdir" ]; then
    echo "art-subdir"
    return
  fi
  echo "none"
}

find "$MUSIC_DIR" -type f \( -iname '*.mp3' -o -iname '*.flac' -o -iname '*.m4a' \
     -o -iname '*.ogg' -o -iname '*.wav' -o -iname '*.aac' -o -iname '*.wma' \) -print0 \
  | xargs -0 -n1 dirname \
  | sort -u \
  | while IFS= read -r dir; do
      found=$(classify_dir "$dir")
      if [ "$found" = "none" ]; then
        parent=$(dirname "$dir")
        parent_found=$(classify_dir "$parent")
        [ "$parent_found" != "none" ] && found="$parent_found (parent dir)"
      fi
      echo "$found"
    done \
  | sort | uniq -c | sort -rn
