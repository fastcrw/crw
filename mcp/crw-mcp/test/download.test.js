// Entrypoint-level coverage for fromDownload(). Testing only digestFor would
// pin the parser rather than the thesis: the point is that a mismatched or
// unverifiable archive is never extracted, and that property lives in
// fromDownload, not in the parser.
//
// Windows is the only platform without a prebuilt package, so this is the code
// path the entire npm Windows install base takes on first run.
const { test } = require("node:test");
const assert = require("node:assert");
const crypto = require("crypto");
const fs = require("fs");
const http = require("http");
const https = require("https");
const os = require("os");
const path = require("path");
const { Readable } = require("stream");

const { fromDownload, cacheDir, plat, binName, proxyAgent } = require("../bin/crw-mcp.js");

// Replace https.get with a queue of canned responses. The launcher holds the
// same module object we mutate here, so this reaches the real code path.
function stubHttps(queue, seen = []) {
  const original = https.get;
  https.get = (url, _opts, cb) => {
    seen.push(url);
    const item = queue.shift();
    const body = item.body === undefined ? [] : [Buffer.from(item.body)];
    const res = Readable.from(body);
    res.statusCode = item.status ?? 200;
    res.headers = {};
    process.nextTick(() => cb(res));
    return { on: () => ({ on: () => {} }) };
  };
  return () => {
    https.get = original;
  };
}

function withTempCache(fn) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-test-"));
  const prev = process.env.XDG_CACHE_HOME;
  const prevLocal = process.env.LOCALAPPDATA;
  process.env.XDG_CACHE_HOME = dir;
  process.env.LOCALAPPDATA = dir;
  return fn(dir).finally(() => {
    if (prev === undefined) delete process.env.XDG_CACHE_HOME;
    else process.env.XDG_CACHE_HOME = prev;
    if (prevLocal === undefined) delete process.env.LOCALAPPDATA;
    else process.env.LOCALAPPDATA = prevLocal;
    fs.rmSync(dir, { recursive: true, force: true });
  });
}

test("a mismatched archive is refused and nothing is cached", async () => {
  await withTempCache(async () => {
    const restore = stubHttps([
      { body: `${"0".repeat(64)}  ${plat.asset}\n` },
      { body: Buffer.from("not the archive you were promised") },
    ]);
    try {
      await assert.rejects(fromDownload(), /checksum mismatch/);
      assert.equal(fs.existsSync(path.join(cacheDir(), binName)), false);
    } finally {
      restore();
    }
  });
});

test("a release with no SHA256SUMS is refused", async () => {
  await withTempCache(async () => {
    const restore = stubHttps([{ status: 404 }]);
    try {
      await assert.rejects(fromDownload(), /publishes no SHA256SUMS/);
    } finally {
      restore();
    }
  });
});

test("a network failure is not reported as a broken release", async () => {
  await withTempCache(async () => {
    const restore = stubHttps([{ status: 500 }]);
    try {
      await assert.rejects(fromDownload(), /could not fetch SHA256SUMS/);
    } finally {
      restore();
    }
  });
});

test("an asset missing from SHA256SUMS is refused", async () => {
  await withTempCache(async () => {
    const restore = stubHttps([
      { body: `${"0".repeat(64)}  some-other-file.tar.gz\n` },
    ]);
    try {
      await assert.rejects(fromDownload(), /not listed in SHA256SUMS/);
    } finally {
      restore();
    }
  });
});



test("a verified archive is extracted and returned as an executable path", async () => {
  // Pins the success path, which the refusal tests above cannot see: without
  // it, collapsing the staging dir, dropping the chmod, or swapping rename for
  // copy all stay green.
  await withTempCache(async () => {
    // 0644 in the archive, so the chmod below is what makes it runnable.
    const archive = buildTarGz(binName, "#!/bin/sh\nexit 0\n", "0000644");
    const digest = crypto.createHash("sha256").update(archive).digest("hex");
    const restore = stubHttps([
      { body: `${digest}  ${plat.asset}\n` },
      { body: archive },
    ]);
    try {
      const resolved = await fromDownload();
      assert.equal(resolved, path.join(cacheDir(), binName));
      assert.ok(fs.existsSync(resolved), "binary should be in the cache");
      if (process.platform !== "win32") {
        assert.ok(fs.statSync(resolved).mode & 0o111, "binary should be executable");
      }
      // Nothing left behind: no staging dir, no archive.
      const leftovers = fs
        .readdirSync(cacheDir())
        .filter((n) => n !== binName);
      assert.deepEqual(leftovers, [], "cache should hold only the binary");
    } finally {
      restore();
    }
  });
});

test("a blocked download host falls back to api.github.com and is still verified", async () => {
  await withTempCache(async () => {
    const archive = buildTarGz(binName, "#!/bin/sh\nexit 0\n");
    const digest = crypto.createHash("sha256").update(archive).digest("hex");
    const release = JSON.stringify({
      assets: [
        { name: "SHA256SUMS", id: 11 },
        { name: plat.asset, id: 22 },
      ],
    });
    const seen = [];
    const restore = stubHttps(
      [
        { status: 500 }, // github.com SHA256SUMS blocked
        { body: release }, // api release lookup
        { body: `${digest}  ${plat.asset}\n` }, // api asset 11
        { status: 500 }, // github.com archive blocked
        { body: release },
        { body: archive }, // api asset 22
      ],
      seen
    );
    try {
      const resolved = await fromDownload();
      assert.ok(fs.existsSync(resolved));
      assert.ok(seen.some((u) => u.endsWith("/releases/assets/22")));
    } finally {
      restore();
    }
  });
});

test("the api fallback still refuses a mismatched archive", async () => {
  await withTempCache(async () => {
    const release = JSON.stringify({
      assets: [
        { name: "SHA256SUMS", id: 11 },
        { name: plat.asset, id: 22 },
      ],
    });
    const restore = stubHttps([
      { status: 500 },
      { body: release },
      { body: `${"0".repeat(64)}  ${plat.asset}\n` },
      { status: 500 },
      { body: release },
      { body: "tampered" },
    ]);
    try {
      await assert.rejects(fromDownload(), /checksum mismatch/);
      assert.equal(fs.existsSync(path.join(cacheDir(), binName)), false);
    } finally {
      restore();
    }
  });
});

test("a 404 is final: the api host is not asked", async () => {
  await withTempCache(async () => {
    const seen = [];
    const restore = stubHttps([{ status: 404 }], seen);
    try {
      await assert.rejects(fromDownload(), /publishes no SHA256SUMS/);
      assert.equal(seen.length, 1);
    } finally {
      restore();
    }
  });
});

test("HTTPS_PROXY routes the download through a CONNECT tunnel", async () => {
  const targets = [];
  const proxy = http.createServer();
  proxy.on("connect", (req, socket) => {
    targets.push(req.url);
    socket.end("HTTP/1.1 502 Bad Gateway\r\n\r\n");
  });
  await new Promise((r) => proxy.listen(0, "127.0.0.1", r));
  const prev = process.env.HTTPS_PROXY;
  process.env.HTTPS_PROXY = `http://127.0.0.1:${proxy.address().port}`;
  try {
    await withTempCache(async () => {
      await assert.rejects(fromDownload(), /proxy CONNECT failed: HTTP 502/);
    });
    assert.equal(targets[0], "github.com:443");
  } finally {
    if (prev === undefined) delete process.env.HTTPS_PROXY;
    else process.env.HTTPS_PROXY = prev;
    proxy.close();
  }
});

test("no proxy variable means no proxy agent", () => {
  const saved = {};
  for (const k of ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]) {
    saved[k] = process.env[k];
    delete process.env[k];
  }
  try {
    assert.equal(proxyAgent(), undefined);
  } finally {
    for (const [k, v] of Object.entries(saved)) if (v !== undefined) process.env[k] = v;
  }
});

// Build a real .tar.gz in-process so the extraction path runs for real.
function buildTarGz(name, content, mode = "0000755") {
  const body = Buffer.from(content);
  const header = Buffer.alloc(512);
  header.write(name, 0, 100);
  header.write(mode + "\0", 100, 8);
  header.write("0000000\0", 108, 8);
  header.write("0000000\0", 116, 8);
  header.write(body.length.toString(8).padStart(11, "0") + "\0", 124, 12);
  header.write("00000000000\0", 136, 12);
  header.write("        ", 148, 8);
  header.write("0", 156, 1);
  header.write("ustar\0" + "00", 257, 8);
  let sum = 0;
  for (const b of header) sum += b;
  header.write(sum.toString(8).padStart(6, "0") + "\0 ", 148, 8);

  const pad = Buffer.alloc((512 - (body.length % 512)) % 512);
  const tar = Buffer.concat([header, body, pad, Buffer.alloc(1024)]);
  return require("zlib").gzipSync(tar);
}
