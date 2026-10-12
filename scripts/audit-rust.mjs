import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { auditedPatchedAlacritty } from "./patched-alacritty.mjs";
import { auditedPatchedGlib, isAuditedPatchedGlib, PATCHED_GLIB_ADVISORY_IDS } from "./patched-glib.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// Every manifest whose locked dependency graph is audited: the Tauri app and the native GPUI shell.
export const AUDITED_MANIFESTS = Object.freeze([
  resolve(ROOT, "src-tauri/Cargo.toml"),
  resolve(ROOT, "native/Cargo.toml"),
]);
export const CARGO_REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
const OSV_ENDPOINT = "https://api.osv.dev/v1/querybatch";
const OSV_VULN_ENDPOINT = "https://api.osv.dev/v1/vulns/";
const BATCH_SIZE = 100;
const DETAIL_CONCURRENCY = 8;
const REQUEST_TIMEOUT_MS = 20_000;

export function cargoMetadataArgs(manifestPath) {
  return ["metadata", "--locked", "--format-version", "1", "--all-features", "--manifest-path", manifestPath];
}

export function parseCargoMetadata(json) {
  if (!json || !Array.isArray(json.packages) || !Array.isArray(json.workspace_members)) {
    throw new Error("Cargo metadata returned malformed package data");
  }

  const workspaceMembers = new Set(json.workspace_members);
  if (json.workspace_members.some((id) => typeof id !== "string")) {
    throw new Error("Cargo metadata returned malformed workspace data");
  }

  const packages = new Map();
  for (const pkg of json.packages) {
    if (
      !pkg ||
      typeof pkg.id !== "string" ||
      typeof pkg.name !== "string" ||
      typeof pkg.version !== "string" ||
      !Object.hasOwn(pkg, "source") ||
      (pkg.source !== null && typeof pkg.source !== "string")
    ) {
      throw new Error("Cargo metadata returned malformed package data");
    }
    const label = safe(`${pkg.name}@${pkg.version}`);
    if (typeof pkg.source === "string" && pkg.source.startsWith("registry+")) {
      if (pkg.source !== CARGO_REGISTRY_SOURCE) {
        throw new Error(`unsupported registry source for ${label}`);
      }
      const key = `${pkg.source}\0${pkg.name}@${pkg.version}`;
      packages.set(key, { name: pkg.name, version: pkg.version, source: pkg.source });
    } else if (pkg.source !== null) {
      throw new Error(`unsupported external dependency source for ${label}`);
    } else {
      const audited = auditedPatchedGlib(pkg) ?? auditedPatchedAlacritty(pkg);
      if (audited) {
        const key = `patched\0${pkg.name}@${pkg.version}`;
        packages.set(key, audited);
      } else if (!workspaceMembers.has(pkg.id)) {
        throw new Error(`unsupported external path dependency for ${label}`);
      }
    }
  }
  return [...packages.values()].sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
}

export function loadLockedPackages({ manifestPaths = AUDITED_MANIFESTS, run = spawnSync } = {}) {
  const packages = new Map();
  for (const manifestPath of manifestPaths) {
    const result = run("cargo", cargoMetadataArgs(manifestPath), {
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
      timeout: 120_000,
    });
    if (result.error || result.status !== 0) {
      throw new Error("cargo metadata could not validate Cargo.lock");
    }

    let metadata;
    try {
      metadata = JSON.parse(result.stdout);
    } catch {
      throw new Error("cargo metadata returned invalid JSON");
    }
    // Both manifests resolve their own share of the dependency graph, so a package reachable from
    // more than one of them is audited once.
    for (const pkg of parseCargoMetadata(metadata)) {
      packages.set(`${pkg.source ?? "patched"}\0${pkg.name}@${pkg.version}`, pkg);
    }
  }
  return [...packages.values()].sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
}

export function classifySeverity(vulnerability) {
  const severity = String(vulnerability.database_specific?.severity ?? "")
    .trim()
    .toUpperCase();
  const known = {
    INFORMATIONAL: "INFO",
    INFO: "INFO",
    NONE: "INFO",
    LOW: "LOW",
    MODERATE: "MODERATE",
    MEDIUM: "MODERATE",
    HIGH: "HIGH",
    CRITICAL: "CRITICAL",
  };
  if (known[severity]) return known[severity];
  if (Array.isArray(vulnerability.severity) && vulnerability.severity.length > 0) {
    return "UNKNOWN";
  }
  if (vulnerability.database_specific?.informational) return "INFO";
  // RustSec writes maintenance status two ways: "foo is unmaintained" and "foo - no longer
  // maintained". Both are the same advisory class, and both used to be classified UNKNOWN when
  // only the first phrasing was recognised, which blocks the build on a non-vulnerability.
  if (/\b(?:unmaintained|no longer maintained)\b/i.test(vulnerability.summary ?? "")) return "INFO";
  return "UNKNOWN";
}

const severityRank = { INFO: 0, LOW: 1, UNKNOWN: 1.5, MODERATE: 2, HIGH: 3, CRITICAL: 4 };

export function isBlocking(advisory) {
  return (
    advisory.status !== "FIXED" &&
    (advisory.severity === "UNKNOWN" || severityRank[advisory.severity] >= severityRank.MODERATE)
  );
}

function isVerifiedGlibBackport(vulnerability, packages) {
  if (!packages.length || !packages.every(isAuditedPatchedGlib)) return false;
  const identifiers = new Set([vulnerability.id, ...(vulnerability.aliases ?? [])].map((id) => id.toUpperCase()));
  return (
    identifiers.size === PATCHED_GLIB_ADVISORY_IDS.length &&
    PATCHED_GLIB_ADVISORY_IDS.every((id) => identifiers.has(id))
  );
}

function mergeAdvisories(records) {
  const groups = [];
  const byIdentifier = new Map();

  for (const { vulnerability, packages, verifiedFix } of records) {
    if (!vulnerability || typeof vulnerability.id !== "string" || !vulnerability.id.trim()) {
      throw new Error("OSV returned an advisory without an identifier");
    }
    if (
      vulnerability.aliases !== undefined &&
      (!Array.isArray(vulnerability.aliases) || vulnerability.aliases.some((id) => typeof id !== "string"))
    ) {
      throw new Error("OSV returned malformed advisory aliases");
    }
    const identifiers = new Set(
      [vulnerability.id, ...(vulnerability.aliases ?? [])]
        .filter((id) => typeof id === "string" && id.trim())
        .map((id) => id.trim().toUpperCase()),
    );
    const matches = [...new Set([...identifiers].map((id) => byIdentifier.get(id)).filter(Boolean))];
    const group = matches[0] ?? {
      identifiers: new Set(),
      packages: new Set(),
      severity: "INFO",
      active: true,
      verifiedFix,
    };
    if (matches.length === 0) groups.push(group);
    for (const duplicate of matches.slice(1)) {
      for (const id of duplicate.identifiers) group.identifiers.add(id);
      for (const name of duplicate.packages) group.packages.add(name);
      if (severityRank[duplicate.severity] > severityRank[group.severity]) {
        group.severity = duplicate.severity;
      }
      group.verifiedFix &&= duplicate.verifiedFix;
      duplicate.active = false;
    }
    group.verifiedFix &&= verifiedFix;
    for (const id of identifiers) {
      group.identifiers.add(id);
      byIdentifier.set(id, group);
    }
    for (const pkg of packages) group.packages.add(`${pkg.name}@${pkg.version}`);
    const severity = classifySeverity(vulnerability);
    if (severityRank[severity] > severityRank[group.severity]) group.severity = severity;
  }

  return groups
    .filter((group) => group.active)
    .map((group) => ({
      identifiers: [...group.identifiers].sort(),
      packages: [...group.packages].sort(),
      severity: group.severity,
      status: group.verifiedFix ? "FIXED" : "ACTIVE",
    }));
}

export async function auditPackages(packages, { fetchImpl = globalThis.fetch, timeoutMs = REQUEST_TIMEOUT_MS } = {}) {
  const findings = new Map();
  for (let start = 0; start < packages.length; start += BATCH_SIZE) {
    const batch = packages.slice(start, start + BATCH_SIZE);
    const body = {
      queries: batch.map(({ name, version }) => ({
        package: { name, ecosystem: "crates.io" },
        version,
      })),
    };
    const data = await requestJson(
      OSV_ENDPOINT,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      },
      fetchImpl,
      timeoutMs,
    );
    if (!data || !Array.isArray(data.results) || data.results.length !== batch.length) {
      throw new Error("OSV returned incomplete advisory results");
    }
    for (let index = 0; index < data.results.length; index += 1) {
      const result = data.results[index];
      if (
        !result ||
        typeof result !== "object" ||
        Array.isArray(result) ||
        (result.vulns !== undefined && !Array.isArray(result.vulns))
      ) {
        throw new Error("OSV returned malformed advisory data");
      }
      for (const vulnerability of result.vulns ?? []) {
        if (!vulnerability || typeof vulnerability.id !== "string" || !/^[A-Za-z0-9._-]+$/.test(vulnerability.id)) {
          throw new Error("OSV returned an advisory without a valid identifier");
        }
        const id = vulnerability.id.toUpperCase();
        const finding = findings.get(id) ?? { id: vulnerability.id, packages: new Set() };
        finding.packages.add(batch[index]);
        findings.set(id, finding);
      }
    }
  }

  const entries = [...findings.values()];
  const records = new Array(entries.length);
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(DETAIL_CONCURRENCY, entries.length) }, async () => {
      while (next < entries.length) {
        const index = next++;
        const { id, packages: affected } = entries[index];
        const vulnerability = await requestJson(
          `${OSV_VULN_ENDPOINT}${encodeURIComponent(id)}`,
          { method: "GET" },
          fetchImpl,
          timeoutMs,
        );
        if (
          !vulnerability ||
          typeof vulnerability !== "object" ||
          Array.isArray(vulnerability) ||
          typeof vulnerability.id !== "string" ||
          (vulnerability.aliases !== undefined &&
            (!Array.isArray(vulnerability.aliases) || vulnerability.aliases.some((alias) => typeof alias !== "string")))
        ) {
          throw new Error("OSV returned malformed advisory details");
        }
        if (
          vulnerability.id.toUpperCase() !== id.toUpperCase() &&
          !vulnerability.aliases?.some((alias) => alias.toUpperCase() === id.toUpperCase())
        ) {
          throw new Error("OSV returned malformed advisory details");
        }
        const affectedPackages = [...affected];
        records[index] = {
          vulnerability,
          packages: affectedPackages,
          verifiedFix: isVerifiedGlibBackport(vulnerability, affectedPackages),
        };
      }
    }),
  );
  return mergeAdvisories(records);
}

async function requestJson(url, init, fetchImpl, timeoutMs) {
  // An owned timer rather than AbortSignal.timeout, whose timer is unref-ed and so holds no handle: a
  // request still waiting on the timeout would let the event loop drain, and the CLI would exit 0
  // having audited nothing.
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const signal = controller.signal;
  try {
    let response;
    try {
      response = await fetchImpl(url, { ...init, signal });
    } catch {
      throw new Error(signal.aborted ? "OSV request timed out" : "OSV request failed (network or TLS error)");
    }
    if (!response.ok) throw new Error(`OSV request failed (HTTP ${response.status})`);
    try {
      return await response.json();
    } catch {
      throw new Error("OSV returned invalid JSON");
    }
  } finally {
    clearTimeout(timer);
  }
}

function safe(value) {
  return String(value).replace(/[^A-Za-z0-9_.@+-]/g, "_");
}

function report(packages, advisories) {
  console.log(`Rust audit: queried ${packages.length} locked Cargo package versions against OSV.`);
  for (const advisory of advisories) {
    const ids = advisory.identifiers;
    const primary = ids.find((id) => id.startsWith("RUSTSEC-")) ?? ids[0];
    const aliases = ids.filter((id) => id !== primary);
    const label = `${safe(primary)}${aliases.length ? ` (${aliases.map(safe).join(", ")})` : ""}`;
    const line = `${advisory.severity} ${label}: ${advisory.packages.map(safe).join(", ")}`;
    if (advisory.status === "FIXED") console.log(`FIXED ${line} (pinned glib backport verified)`);
    else if (isBlocking(advisory)) console.error(`BLOCK ${line}`);
    else console.warn(`WARN ${line}`);
  }

  const blocking = advisories.filter(isBlocking).length;
  const fixed = advisories.filter(({ status }) => status === "FIXED").length;
  const informational = advisories.length - blocking - fixed;
  if (blocking) {
    console.error(`Rust audit blocked: ${blocking} security advisory/advisories require resolution.`);
    return false;
  }
  if (fixed) console.log(`Rust audit verified ${fixed} fixed upstream advisory/advisories in pinned source.`);
  if (informational) {
    console.log(
      `Rust audit completed with ${informational} informational/low advisory/advisories; none were silently suppressed.`,
    );
  } else if (!fixed) {
    console.log("Rust audit completed: no advisories found.");
  }
  return true;
}

async function main() {
  try {
    const packages = loadLockedPackages();
    const advisories = await auditPackages(packages);
    if (!report(packages, advisories)) process.exitCode = 1;
  } catch (error) {
    console.error(`Rust audit could not verify locked dependencies: ${error.message}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
