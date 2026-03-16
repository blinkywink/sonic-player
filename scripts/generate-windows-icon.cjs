/**
 * Generate build/icon.ico from public/icon.png with multiple resolutions
 * so Windows taskbar and title bar look sharp at all DPI.
 */
const path = require("path");
const fs = require("fs");

const SIZES = [16, 24, 32, 48, 64, 128, 256];

async function main() {
  let sharp, toIco;
  try {
    sharp = require("sharp");
    toIco = require("to-ico");
  } catch (e) {
    console.error("Run: npm install --save-dev sharp to-ico");
    process.exit(1);
  }

  const projectRoot = path.join(__dirname, "..");
  const src = path.join(projectRoot, "public", "icon.png");
  const outDir = path.join(projectRoot, "build");
  const outFile = path.join(outDir, "icon.ico");

  if (!fs.existsSync(src)) {
    console.error("Missing public/icon.png");
    process.exit(1);
  }

  const buffers = await Promise.all(
    SIZES.map((size) =>
      sharp(src)
        .resize(size, size)
        .png()
        .toBuffer()
    )
  );

  const ico = await toIco(buffers);
  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(outFile, ico);
  console.log("Wrote build/icon.ico with sizes:", SIZES.join(", "));
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
