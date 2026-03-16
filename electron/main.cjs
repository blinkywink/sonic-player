const { app, Tray, Menu, BrowserWindow, nativeImage, ipcMain } = require("electron");
const path = require("path");
const fs = require("fs");
const { pathToFileURL } = require("url");
const MediaService = require("@arcanewizards/electron-media-service");

// Work around V8/GPU crash on macOS 26 (Tahoe) - see docs/MACOS_26_CRASH.md
if (process.platform === "darwin") {
  const ver = parseInt(process.env.OS_VERSION || require("os").release().split(".")[0], 10) || 0;
  if (ver >= 25) {
    app.commandLine.appendSwitch("disable-gpu");
    app.commandLine.appendSwitch("disable-gpu-sandbox");
    app.commandLine.appendSwitch("disable-software-rasterizer");
    app.commandLine.appendSwitch("disable-gpu-compositing");
    app.commandLine.appendSwitch("js-flags", "--no-turbofan");
  }
}

const PORT = 4000;
let openUrl = `http://localhost:${PORT}`;
let mainWindow = null;

// Debug log to userData so we can see what happens when run from Applications
function debugLog(msg, err) {
  try {
    const dir = app.getPath("userData");
    const logPath = path.join(dir, "sonic-debug.log");
    const line = `[${new Date().toISOString()}] ${msg}${err ? " " + (err.stack || err.message || err) : ""}\n`;
    fs.appendFileSync(logPath, line);
  } catch (_) {}
  if (err) console.error("[Sonic]", msg, err);
  else console.log("[Sonic]", msg);
}

// Only one instance: second launch quits immediately (no extra menu bar icons)
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
  process.exit(0);
}

let serverStarted = false;
let tray = null;
let appIsQuitting = false;
let mediaService = null;

function startServer() {
  if (serverStarted) return;

  const isDev = !app.isPackaged;
  const res = process.resourcesPath;
  const unpacked = path.join(res, "app.asar.unpacked");
  let projectRoot;
  let serverPath;
  let distPath;
  let serverCwd;
  if (isDev) {
    projectRoot = path.join(__dirname, "..");
    serverPath = path.join(projectRoot, "server.js");
    distPath = path.join(projectRoot, "dist");
    serverCwd = projectRoot;
  } else {
    // Mac + Windows packaged: extraResources app/ has out, srv (server.bundle.cjs), public
    projectRoot = path.join(res, "app");
    serverPath = path.join(projectRoot, "srv", "server.bundle.cjs");
    distPath = path.join(projectRoot, "out");
    serverCwd = path.join(projectRoot, "srv");
  }
  debugLog(`packaged=${app.isPackaged} platform=${process.platform} res=${res}`);
  debugLog(`projectRoot=${projectRoot} serverPath=${serverPath}`);
  try {
    projectRoot = fs.realpathSync(projectRoot);
  } catch (e) {
    debugLog("realpath failed", e);
  }

  if (!fs.existsSync(serverPath)) {
    debugLog("Server file missing: " + serverPath);
    debugLog("projectRoot exists: " + fs.existsSync(projectRoot));
    if (fs.existsSync(projectRoot)) {
      try {
        debugLog("app/srv contents: " + fs.readdirSync(path.join(projectRoot, "srv")).join(", "));
      } catch (e2) {
        debugLog("list srv failed", e2);
      }
    }
    return;
  }

  // Run server in this process so env and paths are guaranteed correct.
  process.env.DIST_PATH = distPath;
  process.env.PROJECT_ROOT = projectRoot;
  process.env.DATA_DIR = path.join(app.getPath("userData"), "data");
  process.env.PORT = String(PORT);
  process.env.NODE_ENV = isDev ? "development" : "production";
  process.env.LISTEN_HOST = "127.0.0.1";
  if (!isDev) {
    // Packaged app: server needs discord-rpc. It lives in extraResources app/node_modules (from release-app).
    const appNodeModules = path.join(projectRoot, "node_modules");
    if (fs.existsSync(appNodeModules)) {
      const existing = process.env.NODE_PATH || "";
      process.env.NODE_PATH = existing ? `${appNodeModules}${path.delimiter}${existing}` : appNodeModules;
      debugLog("NODE_PATH=" + process.env.NODE_PATH);
    }
  }
  try {
    process.chdir(serverCwd);
  } catch (e) {
    debugLog("chdir failed", e);
  }

  function onServerReady(mod) {
    if (!mod || !mod.whenListening) {
      debugLog("Server module has no whenListening export");
      return;
    }
    serverStarted = true;
    Promise.resolve(mod.whenListening).then((port) => {
      openUrl = `http://localhost:${port}`;
      updateTrayMenu();
      debugLog("Server running at " + openUrl);
      createOrShowWindow();
    });
  }

  if (serverPath.endsWith(".cjs")) {
    // Load CommonJS bundle (Mac packaged)
    debugLog("Loading server (CJS): " + serverPath);
    try {
      const mod = require(serverPath);
      onServerReady(mod);
    } catch (err) {
      debugLog("Server failed to start", err);
    }
  } else {
    const fileUrl = pathToFileURL(path.resolve(serverPath)).href;
    debugLog("Loading server (ESM): " + fileUrl);
    import(fileUrl)
      .then(onServerReady)
      .catch((err) => debugLog("Server failed to start", err));
  }
}

function stopServer() {
  appIsQuitting = true;
}

function createOrShowWindow() {
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.show();
    mainWindow.focus();
    return;
  }
  mainWindow = new BrowserWindow({
    width: 1200,
    height: 800,
    minWidth: 800,
    minHeight: 500,
    title: "Sonic",
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      preload: path.join(__dirname, "preload.cjs"),
    },
  });
  mainWindow.setMenu(null);
  mainWindow.on("closed", () => {
    mainWindow = null;
    if (process.platform === "darwin" && app.dock && app.setActivationPolicy) {
      app.dock.hide();
      app.setActivationPolicy("accessory");
    }
  });
  mainWindow.on("close", (e) => {
    if (!appIsQuitting) {
      e.preventDefault();
      mainWindow.hide();
      if (process.platform === "darwin" && app.dock && app.setActivationPolicy) {
        app.dock.hide();
        app.setActivationPolicy("accessory");
      }
    }
  });
  mainWindow.on("show", () => {
    if (process.platform === "darwin" && app.dock && app.setActivationPolicy) {
      app.dock.show();
      app.setActivationPolicy("regular");
    }
  });
  mainWindow.loadURL(openUrl).catch((err) => debugLog("Window load failed", err));
  mainWindow.once("ready-to-show", () => {
    if (mainWindow && !mainWindow.isDestroyed()) mainWindow.show();
  });
}

function initMediaService() {
  if (mediaService || process.platform !== "darwin") return;
  try {
    mediaService = new MediaService();
    mediaService.startService();
    debugLog("MediaService started");
    mediaService.on("play", () => {
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("sonic-media:command", { type: "play" });
    });
    mediaService.on("pause", () => {
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("sonic-media:command", { type: "pause" });
    });
    mediaService.on("playPause", () => {
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("sonic-media:command", { type: "playPause" });
    });
    mediaService.on("next", () => {
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("sonic-media:command", { type: "next" });
    });
    mediaService.on("previous", () => {
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("sonic-media:command", { type: "previous" });
    });
    mediaService.on("seek", (to) => {
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("sonic-media:command", { type: "seek", position: to });
    });
    ipcMain.on("sonic-media:set", (_event, meta) => {
      if (!mediaService || typeof mediaService.setMetaData !== "function") return;
      try {
        const m = meta || {};
        const safe = {
          ...m,
          title: m.title != null && String(m.title) !== "undefined" ? String(m.title) : "",
          artist: m.artist != null && String(m.artist) !== "undefined" ? String(m.artist) : "",
          album: m.album != null && String(m.album) !== "undefined" ? String(m.album) : "",
        };
        mediaService.setMetaData(safe);
      } catch (e) {
        debugLog("MediaService setMetaData failed", e);
      }
    });
  } catch (e) {
    debugLog("MediaService init failed", e);
    mediaService = null;
  }
}

function updateTrayMenu() {
  if (!tray || tray.isDestroyed()) return;
  const openAtLogin = app.getLoginItemSettings?.().openAtLogin ?? false;
  const menu = Menu.buildFromTemplate([
    { label: "Open Sonic", click: () => createOrShowWindow() },
    { type: "separator" },
    {
      label: "Run at startup",
      type: "checkbox",
      checked: openAtLogin,
      click: (item) => {
        if (app.setLoginItemSettings) app.setLoginItemSettings({ openAtLogin: item.checked });
      },
    },
    { type: "separator" },
    { label: "Quit", click: () => { appIsQuitting = true; stopServer(); app.quit(); } },
  ]);
  tray.setContextMenu(menu);
}

// 22x22 grey square (base64) – visible on light and dark menu bars
const FALLBACK_ICON = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABYAAAAWCAYAAADEtGw7AAAAUklEQVQ4T2NkYGD4z0ABYBw1gGE0DBgZGP4zMPxnpCDAGUYNGLQGYDgyUhDgowYMAABqVwP5+1TJbQAAAABJRU5ErkJggg==";

function createTray() {
  // Only one tray: destroy any existing one first
  if (tray && !tray.isDestroyed()) {
    tray.destroy();
    tray = null;
  }

  const isDev = !app.isPackaged;
  const res = process.resourcesPath;
  const possibleIconPaths = isDev
    ? [
        path.join(__dirname, "..", "public", "tray-icon.png"),
        path.join(__dirname, "..", "public", "icon.png"),
        path.join(__dirname, "..", "Icon.png"),
        path.join(__dirname, "..", "icon.png"),
      ]
    : [
        path.join(res, "app", "public", "tray-icon.png"),
        path.join(res, "app", "public", "icon.png"),
        path.join(res, "app.asar.unpacked", "public", "tray-icon.png"),
        path.join(res, "app.asar.unpacked", "public", "icon.png"),
        path.join(res, "Icon.png"),
        path.join(res, "icon.png"),
      ];

  let icon = null;
  for (const p of possibleIconPaths) {
    if (fs.existsSync(p)) {
      try {
        const img = nativeImage.createFromPath(p);
        if (!img.isEmpty()) {
          icon = img.resize({ width: 22, height: 22 });
          break;
        }
      } catch (e) {}
    }
  }
  if (!icon || icon.isEmpty()) {
    icon = nativeImage.createFromDataURL(FALLBACK_ICON);
    icon = icon.resize({ width: 22, height: 22 });
  }

  tray = new Tray(icon);
  tray.setToolTip("Sonic");
  updateTrayMenu();
  tray.on("click", () => createOrShowWindow());
}

app.on("second-instance", () => {
  createOrShowWindow();
});

app.whenReady().then(() => {
  if (process.platform === "darwin") {
    if (app.dock) app.dock.hide();
    if (app.setActivationPolicy) app.setActivationPolicy("accessory");
  }
  createTray();
  startServer();
  initMediaService();
});

app.on("before-quit", () => {
  appIsQuitting = true;
  stopServer();
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.destroy();
    mainWindow = null;
  }
  if (tray && !tray.isDestroyed()) {
    tray.destroy();
    tray = null;
  }
});
