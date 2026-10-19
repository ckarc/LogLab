#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Builds WSJT-X's own FT8 tools (jt9 and ft8code) from a pinned commit, to
# serve as the reference the LogLab FT8 bench compares against. WSJT-X is
# GPL-3.0; it is built here as a separate development tool and never ships
# with LogLab.
#
# Needs: git, cmake, a C/C++ compiler, gfortran, FFTW3, Qt 5, Boost, Hamlib,
# PortAudio, libusb (on Ubuntu 24.04: gfortran libfftw3-dev qtbase5-dev
# qttools5-dev qttools5-dev-tools libqt5serialport5-dev qtmultimedia5-dev
# libqt5websockets5-dev libboost-dev libboost-log-dev libboost-filesystem-dev
# libboost-thread-dev libhamlib-dev portaudio19-dev libusb-1.0-0-dev
# libudev-dev libsamplerate0-dev).
#
# Usage: build-wsjtx.sh [WORK_DIR]   (default /var/tmp/loglab-ft8-reference)
set -euo pipefail

WSJTX_REPO="https://github.com/WSJTX/wsjtx"
WSJTX_COMMIT="967c85a6119110788b0df3782cf796485b2cee14" # WSJT-X 3.0.2, 2026-09-23
WORK="${1:-/var/tmp/loglab-ft8-reference}"

mkdir -p "$WORK"
if [ ! -d "$WORK/wsjtx/.git" ]; then
  git clone --quiet "$WSJTX_REPO" "$WORK/wsjtx"
fi
git -C "$WORK/wsjtx" fetch --quiet origin "$WSJTX_COMMIT" 2>/dev/null || true
git -C "$WORK/wsjtx" checkout --quiet "$WSJTX_COMMIT"

cmake -S "$WORK/wsjtx" -B "$WORK/build" -DCMAKE_BUILD_TYPE=Release \
  -DWSJT_GENERATE_DOCS=OFF -DWSJT_SKIP_MANPAGES=ON >"$WORK/cmake.log"
cmake --build "$WORK/build" --target jt9 ft8code -j "$(nproc)" >"$WORK/build.log"
echo "built $WORK/build/jt9 and $WORK/build/ft8code"
