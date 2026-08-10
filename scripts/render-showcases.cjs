const path = require("node:path");
const { pathToFileURL } = require("node:url");

const { chromium } = require("playwright");
const sharp = require("sharp");

const scenes = ["borrow", "move", "conflict", "lifetime"];
const root = path.resolve(__dirname, "..");
const source = path.join(__dirname, "render-showcases.html");
const chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

async function main() {
  const browser = await chromium.launch({ executablePath: chrome, headless: true });
  try {
    const page = await browser.newPage({
      viewport: { width: 1400, height: 600 },
      deviceScaleFactor: 1,
    });
    for (const scene of scenes) {
      const url = new URL(pathToFileURL(source));
      url.searchParams.set("scene", scene);
      await page.goto(url.href, { waitUntil: "load" });
      await page.evaluate(() => document.fonts.ready);
      const png = await page.screenshot({ type: "png" });
      await sharp(png)
        .webp({ quality: 90 })
        .toFile(path.join(root, "assets", "showcase", `zed-rich-${scene}.webp`));
    }
  } finally {
    await browser.close();
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
