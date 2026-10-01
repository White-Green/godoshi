import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { Miniflare } from "miniflare";

const root = fileURLToPath(new URL("../", import.meta.url));
const build = spawnSync(
  "worker-build",
  ["--dev", "--panic-unwind", "--out-dir", "build-tests", "--features", "worker-tests"],
  { cwd: root, stdio: "inherit" },
);
if (build.error) throw build.error;
if (build.status !== 0) process.exit(build.status ?? 1);

const worker = new Miniflare({
  workers: [{
    config: {
      name: "godoshi-tests",
      compatibilityDate: "2026-09-26",
      manifest: {
        mainModule: "build-tests/index.js",
        modules: {
          "build-tests/index.js": {
            type: "esm",
            contents: await readFile(new URL("../build-tests/index.js", import.meta.url), "utf8"),
          },
          "build-tests/index_bg.wasm": {
            type: "wasm",
            contents: new Uint8Array(await readFile(new URL("../build-tests/index_bg.wasm", import.meta.url))),
          },
        },
      },
      env: { TEST_DB: { type: "d1", id: "godoshi-tests" } },
    },
  }],
});

try {
  // One request executes the entire Rust suite in workerd, including database setup.
  const response = await worker.dispatchFetch("http://localhost/test", {
    method: "POST",
  });
  const body = await response.text();
  let report;
  try {
    report = JSON.parse(body);
  } catch {
    throw new Error(`Test endpoint returned HTTP ${response.status}: ${body}`);
  }
  console.log(JSON.stringify(report, null, 2));
  if (!response.ok || report.failed !== 0 || report.passed === 0) process.exitCode = 1;
} finally {
  await worker.dispose();
}
