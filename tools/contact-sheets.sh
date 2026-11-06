#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Zach Jones
#
# Tiles the screenshots produced by `lockstep-headless batch` into labelled contact
# sheets for quick visual review. Requires ImageMagick (`montage`).
#
# usage: tools/contact-sheets.sh <png-dir> <output-prefix> [images-per-sheet]
#   e.g. tools/contact-sheets.sh /tmp/shots /tmp/sheet 48
#        -> /tmp/sheet_00.png, /tmp/sheet_01.png, ...
set -euo pipefail

dir="${1:?png directory}"
out="${2:?output prefix}"
per="${3:-48}"

mapfile -t files < <(find "$dir" -maxdepth 1 -name '*.png' | sort)
n=${#files[@]}
i=0
s=0
while [ "$i" -lt "$n" ]; do
  args=()
  for f in "${files[@]:$i:$per}"; do
    b=$(basename "$f" .png)
    args+=(-label "${b:0:28}" "$f")
  done
  montage "${args[@]}" -tile 8x -geometry 168x158+1+1 -pointsize 11 "${out}_$(printf %02d "$s").png"
  i=$((i + per))
  s=$((s + 1))
done
echo "$s sheet(s) from $n screenshots"
