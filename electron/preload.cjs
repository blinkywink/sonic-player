const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("SonicMedia", {
  setNowPlaying(meta) {
    try {
      ipcRenderer.send("sonic-media:set", meta || {});
    } catch {
      // Ignore if IPC is not available
    }
  },
  onCommand(callback) {
    if (typeof callback !== "function") return;
    ipcRenderer.on("sonic-media:command", (_event, command) => {
      try {
        callback(command);
      } catch {
        // Renderer errors shouldn't break IPC
      }
    });
  },
});
