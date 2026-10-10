import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

export async function buildTestWorker() {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const build = spawnSync(
    "worker-build",
    ["--dev", "--panic-unwind", "--out-dir", "build-tests", "--features", "worker-tests"],
    { cwd: root, stdio: "inherit" },
  );
  if (build.error) throw build.error;
  if (build.status !== 0) process.exit(build.status ?? 1);

  const [source, wasm] = await Promise.all([
    readFile(new URL("../build-tests/index.js", import.meta.url), "utf8"),
    readFile(new URL("../build-tests/index_bg.wasm", import.meta.url)),
  ]);
  return {
    mainModule: "build-tests/index.js",
    modules: {
      "build-tests/index.js": { type: "esm", contents: source },
      "build-tests/index_bg.wasm": { type: "wasm", contents: new Uint8Array(wasm) },
    },
  };
}
