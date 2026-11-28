# usbip-audio (vendored)

Copied from <https://github.com/network-audio/usbip-audio> at commit
`58032ab0664ffec1d0c178f40a3a69359c21f597`. MIT License, Copyright (c) 2026
Stephen von Takach; see `LICENSE`.

It lets the LabLog Agent present a virtual USB sound card to Windows over
USB/IP. The usbip-win2 client driver (installed separately) attaches it, and
Windows' own USB audio driver runs it.

LabLog's changes:

- `Uac2Config::capture_terminal_type` chooses the capture terminal's USB
  terminal type. Upstream always declares a microphone (0x0201). On Windows 11
  25H2 a microphone gets the Voice Clarity effect, which removed a steady 1 kHz
  test tone in shared mode (measured on FL-1, 2026-11-27). A line connector
  (0x0603) is left alone.
- `windows-sys` is 0.61, the version already in the Agent's build. Only
  `timeBeginPeriod` and `timeEndPeriod` are used, and they are unchanged.
- `usbip_exe` (attaching with `usbip.exe`) is left out. The Agent starts
  `usbip.exe` itself, without a console window.
- The upstream examples, integration tests and CI files are not copied.
