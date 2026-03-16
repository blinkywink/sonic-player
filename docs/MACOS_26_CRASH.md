# Sonic crash on macOS 26 (Tahoe)

If Sonic crashes immediately on launch on **macOS 26.x (Tahoe)** with an `EXC_BREAKPOINT (SIGTRAP)` in the crash report, this is a **known Electron/Chromium issue**, not a bug in Sonic itself.

- **What happens:** The crash occurs inside the V8 JavaScript engine during app startup, before any of Sonic’s code runs (e.g. in `SLVerifierHintParametersOf` or similar).
- **Reported upstream:** [electron/electron#49522](https://github.com/electron/electron/issues/49522) – EXC_BREAKPOINT on macOS 26.2 Tahoe during ElectronMain initialization. The issue is open; it affects multiple Electron versions on Tahoe.

**Workarounds:**

1. **Use macOS 15 (Sequoia)** – The same build runs correctly on macOS 15.x.
2. **Upgrade Electron** – This project has been updated to a newer Electron version that may include compatibility fixes. Rebuild the app and try again.
3. **Watch the Electron issue** – When Electron ships a fix for macOS 26, we’ll pick it up via future dependency updates.

If you’re not on macOS 26, this document doesn’t apply; other crashes should be reported with a full crash report.
