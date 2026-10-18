import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { Miniflare } from "miniflare";

const exec = promisify(execFile);
const root = fileURLToPath(new URL("../", import.meta.url));
const bucket = "godoshi-s3-cli-tests";
const database = "00000000-0000-0000-0000-000000000003";
const runId = "998208233077-12345678901234567890";
const nextRunId = "998208233077-9876543210987654321";
const resultKey = (project, key, run = runId) => `projects/${project}/results/${run}/${key}`;
const wrangler = fileURLToPath(new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url));

// Only exposes fixed, trusted service bindings on loopback for the CLI tests.
// The S3 request itself is forwarded unchanged apart from this routing prefix.
const gateway = `export default {
  fetch(request, env) {
    const url = new URL(request.url);
    const match = /^\\/([^/]+)(\\/.*)$/.exec(url.pathname);
    const binding = match && env[match[1]];
    if (!binding) return new Response(null, { status: 404 });
    url.pathname = match[2];
    return binding.fetch(new Request(url, request));
  }
};`;

export async function runS3CliTests(manifest) {
  const target = resolve(root, "target");
  await mkdir(target, { recursive: true });
  const directory = await mkdtemp(join(target, "s3-cli-"));
  const state = join(directory, "state");
  const config = join(directory, "wrangler.json");
  const awsConfig = join(directory, "aws-config");
  const credentials = join(directory, "credentials");
  const certificate = fileURLToPath(new URL("./fixtures/s3-localhost.crt", import.meta.url));
  const env = {
    // Isolate personal AWS settings while exercising CLI defaults.
    ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("AWS_"))),
    AWS_CONFIG_FILE: awsConfig,
    AWS_SHARED_CREDENTIALS_FILE: credentials,
    // Trust only the local HTTPS test fixture; checksum/transfer settings stay default.
    AWS_CA_BUNDLE: certificate,
    PYTHONUTF8: "1",
    NO_PROXY: "127.0.0.1,localhost",
    no_proxy: "127.0.0.1,localhost",
    WRANGLER_SEND_METRICS: "false",
    WRANGLER_LOG_PATH: join(directory, "wrangler.log"),
  };
  await Promise.all([
    writeFile(config, JSON.stringify({
      name: "s3-cli-fixtures", compatibility_date: "2026-09-26",
      d1_databases: [{
        binding: "DB", database_name: "godoshi-s3-cli-tests", database_id: database,
        migrations_dir: fileURLToPath(new URL("../migrations/", import.meta.url)),
      }],
    })),
    writeFile(credentials, ""),
    writeFile(awsConfig, "[default]\n"),
  ]);

  async function command(executable, args, allowFailure = false) {
    try {
      return { ...await exec(executable, args, { cwd: directory, env, windowsHide: true, timeout: 60_000, maxBuffer: 4 * 1024 * 1024 }), code: 0 };
    } catch (error) {
      if (allowFailure && Number.isInteger(error.code) && !error.killed) return error;
      throw new Error(`${executable} ${args.join(" ")}\n${error.stderr ?? error.message}\n${error.stdout ?? ""}`, { cause: error });
    }
  }
  const awsExecutable = process.env.AWS_CLI_PYTHON ?? process.env.AWS_CLI ?? "aws";
  const awsPrefix = process.env.AWS_CLI_PYTHON ? ["-m", "awscli"] : [];
  async function r2(operation, key, file, allowFailure = false, options = []) {
    return command(process.execPath, [wrangler, "r2", "object", operation, `${bucket}/${key}`,
      "--local", "--persist-to", state, "--config", config, "--file", file, ...options], allowFailure);
  }
  async function fixture(name, data) {
    const path = join(directory, name);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, data);
    return path;
  }
  let runtime;
  let passed = 0;
  let success = false;
  async function test(name, run) {
    process.stdout.write(`S3 CLI: ${name} ... `);
    await run();
    passed++;
    console.log("passed");
  }
  const expectedObjects = new Map();
  try {
    const version = await command(awsExecutable, [...awsPrefix, "--version"]);
    console.log((version.stdout || version.stderr).trim());
    console.log(`S3 CLI fixtures: ${directory}`);
    const input = await fixture("input.txt", "project one\n");
    const other = await fixture("other.txt", "project two\n");
    const binary = await fixture("binary.bin", Buffer.from([0, 1, 127, 128, 255]));
    const empty = await fixture("empty", "");
    const large = await fixture("large.bin", Buffer.alloc(11 * 1024 * 1024 + 17, 42));
    const firstPart = await fixture("part.bin", Buffer.alloc(5 * 1024 * 1024, 17));

    await test("Wrangler applies D1 migrations", async () => {
      await command(process.execPath, [wrangler, "d1", "migrations", "apply", "DB",
        "--local", "--persist-to", state, "--config", config]);
    });
    // Wrangler and Miniflare use the same persistence root. Run Wrangler before
    // starting the endpoint and after stopping it so only one runtime owns R2.
    await test("Wrangler seeds physical R2 keys", async () => {
      for (const [key, file] of [
        ["projects/1/resources/input.txt", input],
        ["projects/2/resources/input.txt", other],
        ["projects/1/resources/environment.closure_zstd", binary],
        // Wrangler's local PUT stores URL-encoded paths. Use ASCII seed keys;
        // Unicode uploads are made with AWS CLI and checked by Wrangler GET below.
        ["projects/1/resources/nested/a+%2F.bin", binary],
        ["projects/1/resources/empty", empty],
        ["projects/1/resources/list/a&b.txt", input],
        ["projects/1/resources/list/b.txt", other],
        ["projects/1/resources/list/dir/c.txt", input],
        ["projects/2/resources/list/secret.txt", other],
        ["projects/1/results/list/secret.txt", other],
      ]) {
        await r2("put", key, file, false, ["--content-type", "application/custom", "--content-language", "ja"]);
      }
    });
    const binding = (props) => ({ type: "worker", worker: "s3-api", exportName: "OutboundHandler", ...(props && { props }) });
    const runtimeOptions = {
      host: "127.0.0.1", port: 0, telemetry: { enabled: false },
      resourcePersistencePath: join(state, "v3"),
      workers: [
        { config: {
          name: "s3-cli-gateway", compatibilityDate: "2026-09-26",
          manifest: { mainModule: "gateway.js", modules: { "gateway.js": { type: "esm", contents: gateway } } },
          env: {
            p1: binding({ project_id: 1, run_id: runId }),
            p2: binding({ project_id: 2, run_id: runId }),
            p1next: binding({ project_id: 1, run_id: nextRunId }),
            missing: binding(),
            missingRun: binding({ project_id: 1 }),
            bad: binding({ project_id: "../2", run_id: runId }),
          },
        } },
        { config: {
          name: "s3-api", compatibilityDate: "2026-09-26", manifest,
          env: { BUCKET: { type: "r2", name: bucket }, DB: { type: "d1", id: database } },
        } },
      ],
    };
    runtime = new Miniflare(runtimeOptions);
    let origin = (await runtime.ready).origin;
    const db = await runtime.getD1Database("DB", "s3-api");
    async function aws(project, args, allowFailure = false) {
      return command(awsExecutable, [...awsPrefix, "--endpoint-url", `${origin}/${project}`, "--no-sign-request", ...args], allowFailure);
    }
    async function api(project, operation, args = []) {
      const { stdout } = await aws(project, ["s3api", operation, ...args]);
      return stdout.trim() ? JSON.parse(stdout) : undefined;
    }
    async function rejected(project, args, code) {
      const result = await aws(project, args, true);
      assert.notEqual(result.code, 0, `Unexpected success: aws ${args.join(" ")}`);
      assert.ok(result.stderr.includes(`(${code})`), result.stderr);
      assert.ok(!result.stderr.includes("projects/"), result.stderr);
    }
    async function compareFile(actual, expected) {
      assert.deepEqual(await readFile(actual), await readFile(expected));
    }
    async function assertUploadRemoved(uploadId) {
      assert.equal(await db.prepare("SELECT count(*) FROM s3_multipart_upload WHERE r2_upload_id = ?").bind(uploadId).first("count(*)"), 0);
      assert.equal(await db.prepare(`SELECT count(*) FROM s3_multipart_part p WHERE NOT EXISTS (
        SELECT 1 FROM s3_multipart_upload u WHERE u.object_key = p.object_key AND u.r2_upload_id = p.r2_upload_id
      )`).first("count(*)"), 0);
    }

    await test("aws s3 cp downloads scoped/escaped keys; empty GET/HEAD", async () => {
      for (const [project, key, expected] of [
        ["p1", "input.txt", input], ["p2", "input.txt", other], ["p1next", "input.txt", input],
        ["p1", "environment.closure_zstd", binary],
        ["p1", "nested/a+%2F.bin", binary],
      ]) {
        const output = join(directory, "download.bin");
        await aws(project, ["s3", "cp", `s3://resources/${key}`, output, "--only-show-errors"]);
        await compareFile(output, expected);
      }
      const output = join(directory, "empty-download");
      await aws("p1", ["s3", "cp", "s3://resources/empty", output]);
      await compareFile(output, empty);
      const head = await api("p1", "head-object", ["--bucket", "resources", "--key", "empty"]);
      assert.equal(head.ContentLength, 0);
    });
    await test("HEAD, metadata, ranges and ETag conditions", async () => {
      for (const name of ["resources", "results"]) await api("p1", "head-bucket", ["--bucket", name]);
      const head = await api("p1", "head-object", ["--bucket", "resources", "--key", "input.txt"]);
      assert.equal(head.ContentLength, (await readFile(input)).length);
      assert.equal(head.ContentType, "application/custom");
      assert.equal(head.ContentLanguage, "ja");
      assert.ok(head.ETag && head.LastModified);
      const output = join(directory, "range.txt");
      const range = await api("p1", "get-object", ["--bucket", "resources", "--key", "input.txt", "--range", "bytes=0-6", "--if-match", head.ETag, output]);
      assert.equal(range.ContentRange, "bytes 0-6/12");
      assert.equal(await readFile(output, "utf8"), "project");
      await rejected("p1", ["s3api", "get-object", "--bucket", "resources", "--key", "input.txt", "--if-match", '"wrong"', output], "PreconditionFailed");
      await rejected("p1", ["s3api", "get-object", "--bucket", "resources", "--key", "input.txt", "--if-none-match", head.ETag, output], "304");
      await rejected("p1", ["s3api", "get-object", "--bucket", "resources", "--key", "input.txt", "--range", "bytes=999-", output], "InvalidRange");
    });
    await test("aws s3 ls/sync and ListObjectsV2 pagination", async () => {
      const listing = (await aws("p1", ["s3", "ls", "s3://resources/list/", "--recursive", "--page-size", "1"])).stdout;
      assert.ok(listing.includes("list/a&b.txt") && listing.includes("list/dir/c.txt"), listing);
      assert.ok(!listing.includes("secret") && !listing.includes("projects/"), listing);
      const downloads = join(directory, "sync");
      await aws("p1", ["s3", "sync", "s3://resources/list/", downloads, "--page-size", "1", "--only-show-errors"]);
      await compareFile(join(downloads, "a&b.txt"), input);
      await compareFile(join(downloads, "b.txt"), other);
      await compareFile(join(downloads, "dir", "c.txt"), input);
      const first = await api("p1", "list-objects-v2", ["--bucket", "resources", "--prefix", "list/", "--max-keys", "1", "--no-paginate"]);
      assert.equal(first.IsTruncated, true);
      assert.equal(first.Contents[0].Key, "list/a&b.txt");
      const next = await api("p1", "list-objects-v2", ["--bucket", "resources", "--prefix", "list/", "--continuation-token", first.NextContinuationToken]);
      assert.deepEqual(next.Contents.map((o) => o.Key), ["list/b.txt", "list/dir/c.txt"]);
      const folders = await api("p1", "list-objects-v2", ["--bucket", "resources", "--prefix", "list/", "--delimiter", "/", "--start-after", "list/b.txt"]);
      assert.deepEqual(folders.CommonPrefixes, [{ Prefix: "list/dir/" }]);
    });
    await test("aws s3 cp uploads, overwrites and isolates project/run results", async () => {
      for (const [project, file] of [["p1", input], ["p2", other], ["p1next", input], ["p1", binary]]) {
        await aws(project, ["s3", "cp", file, "s3://results/shared.bin", "--metadata", "origin=aws-cli", "--only-show-errors"]);
      }
      await aws("p1", ["s3", "cp", empty, "s3://results/empty", "--only-show-errors"]);
      await aws("p1", ["s3", "cp", binary, "s3://results/nested/日本 +%2F.bin", "--only-show-errors"]);
      expectedObjects.set(resultKey(1, "shared.bin"), binary);
      expectedObjects.set(resultKey(2, "shared.bin"), other);
      expectedObjects.set(resultKey(1, "shared.bin", nextRunId), input);
      expectedObjects.set(resultKey(1, "empty"), empty);
      expectedObjects.set(resultKey(1, "nested/日本 +%2F.bin"), binary);
    });
    await test("aws s3 cp performs an automatic multipart upload", async () => {
      await aws("p1", ["s3", "cp", large, "s3://results/large.bin", "--only-show-errors"]);
      expectedObjects.set(resultKey(1, "large.bin"), large);
    });
    await test("explicit multipart upload, project/run isolation and completion", async () => {
      const created = await api("p1", "create-multipart-upload", ["--bucket", "results", "--key", "multipart.bin", "--content-type", "application/custom"]);
      const { UploadId } = created;
      const stored = await db.prepare(`SELECT algorithm, typeof(algorithm) AS algorithm_type
        FROM s3_multipart_upload WHERE r2_upload_id = ?`).bind(UploadId).first();
      assert.ok([null, 1, 2].includes(stored.algorithm));
      assert.equal(stored.algorithm_type, stored.algorithm === null ? "null" : "integer");
      if (stored.algorithm !== null) {
        assert.equal(stored.algorithm, { CRC32: 1, CRC64NVME: 2 }[created.ChecksumAlgorithm]);
      }
      const upload = ["--bucket", "results", "--key", "multipart.bin", `--upload-id=${UploadId}`];
      await rejected("p2", ["s3api", "upload-part", ...upload, "--part-number", "1", "--body", firstPart], "NoSuchUpload");
      await rejected("p1next", ["s3api", "upload-part", ...upload, "--part-number", "1", "--body", firstPart], "NoSuchUpload");
      await rejected("p1", ["s3api", "upload-part", "--bucket", "results", "--key", "wrong.bin", `--upload-id=${UploadId}`, "--part-number", "1", "--body", binary], "NoSuchUpload");
      // Replacing a part must retain the metadata associated with each ETag.
      const replaced = await api("p1", "upload-part", [...upload, "--part-number", "1", "--body", binary]);
      const parts = [];
      for (const [number, file] of [[1, firstPart], [2, binary]]) {
        const { ETag } = await api("p1", "upload-part", [...upload, "--part-number", String(number), "--body", file]);
        parts.push({ PartNumber: number, ETag });
      }
      const versions = await db.prepare(`SELECT etag, algorithm, typeof(algorithm) AS algorithm_type FROM s3_multipart_part
        WHERE object_key = ? AND r2_upload_id = ? AND part_number = 1`).bind(resultKey(1, "multipart.bin"), UploadId).all();
      assert.deepEqual(versions.results.map(({ etag }) => etag).sort(), [replaced.ETag, parts[0].ETag].map((etag) => etag.replaceAll('"', "")).sort());
      for (const row of versions.results) {
        assert.equal(row.algorithm_type, "integer");
        assert.ok([1, 2].includes(row.algorithm));
      }
      const partsFile = await fixture("parts.json", JSON.stringify({ Parts: parts }));
      const completed = await api("p1", "complete-multipart-upload", [...upload, "--multipart-upload", `file://${partsFile}`]);
      assert.equal(completed.Key, "multipart.bin");
      assert.equal(completed.Bucket, "results");
      await assertUploadRemoved(UploadId);
      const expected = await fixture("multipart-expected.bin", Buffer.concat([await readFile(firstPart), await readFile(binary)]));
      expectedObjects.set(resultKey(1, "multipart.bin"), expected);
    });
    await test("multipart abort and invalid parts", async () => {
      const { UploadId } = await api("p1", "create-multipart-upload", ["--bucket", "results", "--key", "aborted.bin"]);
      const upload = ["--bucket", "results", "--key", "aborted.bin", `--upload-id=${UploadId}`];
      const missing = await fixture("missing-parts.json", JSON.stringify({ Parts: [{ PartNumber: 1, ETag: '"missing"' }] }));
      await rejected("p1", ["s3api", "complete-multipart-upload", ...upload, "--multipart-upload", `file://${missing}`], "InvalidPart");
      await api("p1", "upload-part", [...upload, "--part-number", "1", "--body", binary]);
      await api("p1", "abort-multipart-upload", upload);
      await assertUploadRemoved(UploadId);
      await rejected("p1", ["s3api", "upload-part", ...upload, "--part-number", "1", "--body", binary], "NoSuchUpload");
    });
    await test("access policy, invalid props and unsupported operations", async () => {
      for (const name of ["resource", "result"]) await rejected("p1", ["s3api", "head-bucket", "--bucket", name], "403");
      await rejected("p1", ["s3api", "get-object", "--bucket", "resource", "--key", "input.txt", join(directory, "forbidden")], "AccessDenied");
      await rejected("p1", ["s3", "cp", input, "s3://result/forbidden.txt", "--only-show-errors"], "AccessDenied");
      await rejected("p1", ["s3", "cp", input, "s3://resources/forbidden.txt", "--only-show-errors"], "AccessDenied");
      await rejected("p1", ["s3api", "get-object", "--bucket", "results", "--key", "shared.bin", join(directory, "forbidden")], "AccessDenied");
      await rejected("p1", ["s3", "ls", "s3://results/"], "AccessDenied");
      await rejected("p1", ["s3api", "create-multipart-upload", "--bucket", "resources", "--key", "forbidden.txt"], "AccessDenied");
      for (const project of ["missing", "missingRun", "bad"]) await rejected(project, ["s3", "cp", input, "s3://results/forbidden.txt", "--only-show-errors"], "AccessDenied");
      await rejected("p1", ["s3", "rm", "s3://resources/input.txt", "--only-show-errors"], "NotImplemented");
      await rejected("p1", ["s3api", "get-object", "--bucket", "resources", "--key", "absent", join(directory, "absent")], "NoSuchKey");
    });
    // AWS CLI uses header checksums over HTTP, and aws-chunked trailers over
    // HTTPS. Exercise both default transports against the same handler.
    await runtime.setOptions({
      ...runtimeOptions,
      https: true,
      httpsCert: await readFile(certificate, "utf8"),
      httpsKey: await readFile(new URL("./fixtures/s3-localhost.key", import.meta.url), "utf8"),
    });
    origin = (await runtime.ready).origin;
    await test("aws s3 cp uploads small/empty/multipart files over HTTPS", async () => {
      for (const [name, file] of [["small", input], ["empty", empty], ["multipart", large]]) {
        await aws("p1", ["s3", "cp", file, `s3://results/https-${name}`]);
        expectedObjects.set(resultKey(1, `https-${name}`), file);
      }
    });

    await test("multipart metadata is cleaned up in D1 without R2 sidecars", async () => {
      const physicalDb = await runtime.getD1Database("DB", "s3-api");
      for (const table of ["s3_multipart_upload", "s3_multipart_part"]) {
        assert.equal(await physicalDb.prepare(`SELECT count(*) FROM ${table}`).first("count(*)"), 0);
      }
      const physicalR2 = await runtime.getR2Bucket("BUCKET", "s3-api");
      for (const project of [1, 2]) {
        assert.deepEqual((await physicalR2.list({ prefix: `projects/${project}/.s3/` })).objects, []);
      }
    });

    await runtime.dispose();
    runtime = undefined;
    await test("Wrangler verifies physical R2 data and forbidden writes", async () => {
      expectedObjects.set("projects/1/resources/input.txt", input);
      for (const [key, expected] of expectedObjects) {
        const output = join(directory, "r2-object.bin");
        await r2("get", key, output);
        await compareFile(output, expected);
      }
      for (const key of [
        "results/shared.bin", "projects/1/result/shared.bin", "projects/1/result/forbidden.txt",
        "projects/1/resources/forbidden.txt", "projects/1/results/forbidden.txt",
        "projects/1/results/shared.bin", "projects/2/results/shared.bin", "projects/1/results/multipart.bin",
        resultKey(1, "forbidden.txt"), resultKey(1, "aborted.bin"), resultKey(2, "multipart.bin"),
        resultKey(1, "multipart.bin", nextRunId),
      ]) {
        const response = await r2("get", key, join(directory, "absent-object"), true);
        assert.notEqual(response.code, 0, `Unexpected R2 object: ${key}`);
        assert.match(response.stderr, /specified key does not exist/);
      }
    });
    console.log(`S3 CLI E2E: ${passed} passed`);
    success = true;
  } finally {
    await runtime?.dispose();
    // Only remove the newly-created test directory, never a configured path.
    assert.equal(dirname(resolve(directory)), target);
    if (success) await rm(directory, { recursive: true, force: true });
    else console.error(`S3 CLI failure artifacts: ${directory}`);
  }
}
