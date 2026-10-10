import { Miniflare } from "miniflare";

export async function runWorkerTests(manifest) {
  const worker = new Miniflare({
    workers: [{
      config: {
        name: "godoshi-tests",
        compatibilityDate: "2026-09-26",
        manifest,
        env: { TEST_DB: { type: "d1", id: "godoshi-tests" } },
      },
    }],
  });

  try {
    // Existing Rust tests cover the application's database/authentication code.
    const response = await worker.dispatchFetch("http://localhost/test", { method: "POST" });
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
}
