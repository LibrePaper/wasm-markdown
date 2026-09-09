// Pre-compresses the built module, so a host serves bytes it did not have to
// compress on the way out. A WebAssembly module is compressed once at build
// time and fetched many times; doing it per request, at the quality a server
// can afford in a request, is both slower and worse.
//
// Brotli is what a browser takes for `Content-Encoding: br`, and gzip is the
// fallback for the few that do not. Quality 11 and a 16 MB window are the most
// the format allows a browser to decode: `lgwin` above 24 is brotli's
// large-window extension, which no browser implements, so 24 is the ceiling
// rather than a tuning choice.
//
// Node's own zlib does both. Nothing is installed for this.

import { readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";
import zlib from "node:zlib";

const files = process.argv.slice(2);
if (files.length === 0) {
  console.error("usage: node tools/compress.mjs <file>...");
  process.exit(2);
}

const kib = (n) => (n / 1024).toFixed(0).padStart(7) + " KiB";
const pct = (part, whole) => ((1 - part / whole) * 100).toFixed(1).padStart(5) + "%";

for (const file of files) {
  const raw = readFileSync(file);

  const started = Date.now();
  const br = zlib.brotliCompressSync(raw, {
    params: {
      [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
      [zlib.constants.BROTLI_PARAM_LGWIN]: 24,
      [zlib.constants.BROTLI_PARAM_SIZE_HINT]: raw.length,
    },
  });
  const brotliSeconds = ((Date.now() - started) / 1000).toFixed(1);
  writeFileSync(`${file}.br`, br);

  const gz = zlib.gzipSync(raw, { level: 9 });
  writeFileSync(`${file}.gz`, gz);

  const name = basename(file);
  console.log(`${name}`);
  console.log(`  raw     ${kib(raw.length)}`);
  console.log(`  brotli  ${kib(br.length)}  ${pct(br.length, raw.length)} smaller  (q11, ${brotliSeconds}s)`);
  console.log(`  gzip    ${kib(gz.length)}  ${pct(gz.length, raw.length)} smaller  (level 9)`);
}
