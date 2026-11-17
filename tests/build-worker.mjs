import { spawnSync } from "node:child_process";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export async function buildTestWorker() {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const build = spawnSync(
    "worker-build",
    ["--locked", "--dev", "--panic-unwind", "--out-dir", "build-tests", "--features", "worker-tests"],
    { cwd: root, stdio: "inherit" },
  );
  if (build.error) throw build.error;
  if (build.status !== 0) process.exit(build.status ?? 1);

  // Include the entry point, wasm-bindgen glue, snippets, and Wasm modules.
  const directory = new URL("../build-tests/", import.meta.url);
  const modules = {};
  for (const file of await readdir(directory, { recursive: true })) {
    const name = `build-tests/${file.replaceAll("\\", "/")}`;
    const path = join(fileURLToPath(directory), file);
    if (/\.m?js$/.test(name)) {
      modules[name] = { type: "esm", contents: await readFile(path, "utf8") };
    } else if (name.endsWith(".wasm")) {
      modules[name] = { type: "wasm", contents: new Uint8Array(await readFile(path)) };
    }
  }
  return { mainModule: "build-tests/index.js", modules };
}
