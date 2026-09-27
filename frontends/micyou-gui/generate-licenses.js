/*
 * libmicyou — micyou-gui frontend.
 * Lightweight replacement for the upstream generator: the original shells out
 * to `cargo-about` (a heavyweight Rust toolchain component) to render the full
 * dependency report. This variant produces the same output file
 * (src/generated/third-party-licenses.html, imported `?raw` by
 * LicensesDialog.vue) from package-lock.json alone, plus a pointer section for
 * the Rust side, whose crates are listed in the repository with their
 */

import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const appDir = path.dirname(fileURLToPath(import.meta.url));
const generatedDir = path.join(appDir, 'src', 'generated');
const outputFile = path.join(generatedDir, 'third-party-licenses.html');
const packageLockFile = path.join(appDir, 'package-lock.json');

await mkdir(generatedDir, { recursive: true });

const escapeHtml = (value) =>
  String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;');

async function readLicenseFiles(packageDirectory) {
  const files = await readdir(packageDirectory).catch(() => []);
  const licenseFiles = files
    .filter((file) => /^(licen[cs]e|copying|notice)(\..*)?$/i.test(file))
    .sort((left, right) => left.localeCompare(right));

  const texts = await Promise.all(
    licenseFiles.map(async (file) => {
      const text = await readFile(path.join(packageDirectory, file), 'utf8').catch(() => '');
      return text.trim() ? `${file}\n\n${text.trim()}` : '';
    }),
  );
  return texts.filter(Boolean).join('\n\n');
}

async function generateNpmReport() {
  const lock = JSON.parse(await readFile(packageLockFile, 'utf8'));
  const seen = new Set();
  const packages = [];

  for (const [packagePath, metadata] of Object.entries(lock.packages ?? {})) {
    if (!packagePath || metadata.dev || !packagePath.includes('node_modules/')) continue;

    const marker = 'node_modules/';
    const name = metadata.name ?? packagePath.slice(packagePath.lastIndexOf(marker) + marker.length);
    const version = metadata.version ?? 'unknown';
    const key = `${name}@${version}`;
    if (seen.has(key)) continue;
    seen.add(key);

    packages.push({
      name,
      version,
      license: metadata.license ?? 'UNKNOWN',
      text: await readLicenseFiles(path.join(appDir, packagePath)),
    });
  }

  packages.sort(
    (left, right) =>
      left.name.localeCompare(right.name) || left.version.localeCompare(right.version),
  );
  console.log(`[licenses] Frontend production dependencies: ${packages.length}`);

  const rows = packages
    .map(
      ({ name, version, license }) => `<tr>
        <td><a href="https://www.npmjs.com/package/${encodeURIComponent(name)}" target="_blank" rel="noreferrer">${escapeHtml(name)}</a></td>
        <td>${escapeHtml(version)}</td>
        <td>${escapeHtml(license)}</td>
      </tr>`,
    )
    .join('\n');
  const details = packages
    .filter(({ text }) => text)
    .map(
      ({ name, version, text }) => `<details>
      <summary>${escapeHtml(name)} ${escapeHtml(version)}</summary>
      <pre>${escapeHtml(text)}</pre>
    </details>`,
    )
    .join('\n');

  return `<section class="npm-licenses">
  <div class="license-summary">
    <h3>Frontend dependencies</h3>
    <p>Generated from production packages in package-lock.json.</p>
  </div>
  <table class="license-table">
    <thead><tr><th>Package</th><th>Version</th><th>License</th></tr></thead>
    <tbody>${rows}</tbody>
  </table>
  <div class="license-texts">${details}</div>
</section>`;
}

function generateBackendReport() {
  const crates = [
    ['micyou-protocol', 'Android wire protocol (protobuf/JSON framing)'],
    ['micyou-audio', 'Capture/playback, DSP chain (NS/AEC/AGC/VAD/EQ)'],
    ['micyou-plugin', 'Plugin manifest, native + wasm runtimes, host bridge'],
    ['micyou-transport', 'TCP control / UDP audio / WebSocket transports, adb, mDNS'],
    ['micyou-core', 'Server core, lifecycle, config, plugin host services'],
    ['micyou-api', 'RPC contract: methods, events, JSON-RPC envelope'],
    ['micyou-rpc', 'JSON-RPC router + stdio/ws/local transports'],
    ['micyou-client', 'Rust client SDK'],
    ['micyou-daemon', 'Headless backend daemon binary (sidecar for this GUI)'],
    ['libmicyou', 'Facade crate for embedding the backend'],
  ];
  const rows = crates
    .map(
      ([name, description]) => `<tr>
        <td><a href="https://github.com/OrientCOMPASS/libmicyou/tree/main/crates/${name}" target="_blank" rel="noreferrer">${escapeHtml(name)}</a></td>
        <td>${escapeHtml(description)}</td>
        <td>License undecided (experimental)</td>
      </tr>`,
    )
    .join('\n');
  return `<section class="backend-licenses">
  <div class="license-summary">
    <h3>Backend (libmicyou)</h3>
    <p>This GUI talks to the <code>micyou-daemon</code> sidecar built from the
    libmicyou workspace. The workspace itself is experimental with its license
    undecided; the third-party Rust dependency licenses are listed in the
    <a href="https://github.com/OrientCOMPASS/libmicyou" target="_blank" rel="noreferrer">libmicyou
    repository</a> (see <code>Cargo.lock</code>).</p>
  </div>
  <table class="license-table">
    <thead><tr><th>Crate</th><th>Role</th><th>License</th></tr></thead>
    <tbody>${rows}</tbody>
  </table>
</section>`;
}

const npmReport = await generateNpmReport();
const backendReport = generateBackendReport();
await writeFile(outputFile, `${backendReport}\n${npmReport}\n`);
console.log(`[licenses] Report written to ${path.relative(appDir, outputFile)}`);
