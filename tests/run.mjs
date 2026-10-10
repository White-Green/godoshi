import { buildTestWorker } from "./build-worker.mjs";
import { runS3CliTests } from "./s3-cli.mjs";
import { runWorkerTests } from "./worker.mjs";

const manifest = await buildTestWorker();
await runWorkerTests(manifest);
await runS3CliTests(manifest);
