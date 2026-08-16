/**
 * Version-skew guard for the project's `vite` alias.
 *
 * `vp create` / `vp migrate` scaffold two entries that must move in lockstep:
 * the `vite-plus` dependency and the `vite` alias
 * (`npm:@voidzero-dev/vite-plus-core@<same version>`). A dependency bot sees
 * two unrelated packages and bumps them in separate PRs, so a project can end
 * up running a CLI/core pairing that was never published together (#2356).
 * The skew is silent: `vp build`/`vp dev`/`vp test` execute the CLI's own
 * core dependency, while plugins and configs that `import 'vite'` load the
 * project's aliased copy at the other version. Fail fast instead, so a
 * mismatched bot PR fails CI before the pairing ships.
 */

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

import { VITE_PLUS_VERSION } from './constants.ts';

export const CORE_PACKAGE_NAME = '@voidzero-dev/vite-plus-core';
export const SKIP_CORE_VERSION_CHECK_ENV = 'VP_SKIP_CORE_VERSION_CHECK';

interface InstalledVitePackage {
  name?: string;
  version?: string;
}

/**
 * Read the `package.json` of whatever `vite` resolves to from the project
 * directory, the same copy the project's plugins and configs import. Returns
 * `null` when `vite` is not resolvable (no install yet, or no `vite`
 * dependency at all).
 *
 * The `_createRequire` / `_readFile` parameters let tests inject controlled
 * resolvers without spying on Node's module/fs namespaces (same pattern as
 * the coverage-provider guard in `define-config.ts`).
 */
export function readProjectVitePackage(
  projectDir: string,
  _createRequire: (from: string) => { resolve: (id: string) => string } = createRequire,
  _readFile: (file: string) => string = (file) => readFileSync(file, 'utf8'),
): InstalledVitePackage | null {
  try {
    const req = _createRequire(path.join(projectDir, 'package.json'));
    const pkgJsonPath = req.resolve('vite/package.json');
    return JSON.parse(_readFile(pkgJsonPath)) as InstalledVitePackage;
  } catch {
    return null;
  }
}

/**
 * Throw when the project's `vite` alias resolves to a
 * `@voidzero-dev/vite-plus-core` whose version differs from the running CLI.
 * A no-op when `vite` is not installed, resolves to real Vite (a project that
 * did not adopt the alias), or matches the CLI version.
 *
 * Exported for unit testing.
 */
export function assertCoreVersionMatch(
  installed: InstalledVitePackage | null,
  expectedVersion: string = VITE_PLUS_VERSION,
): void {
  if (installed?.name !== CORE_PACKAGE_NAME || !installed.version) {
    return;
  }
  if (installed.version !== expectedVersion) {
    // Keep every version inside a `@voidzero-dev/vite-plus-core@<x>` context:
    // the PTY snapshot redactor masks the CLI's own version only in that form
    // (a bare `vite-plus@<x>` stays verbatim and would churn every release).
    throw new Error(
      `The project's \`vite\` alias resolves to ${CORE_PACKAGE_NAME}@${installed.version}, ` +
        `but this vite-plus CLI requires ${CORE_PACKAGE_NAME}@${expectedVersion}: the two ` +
        `packages are published in lockstep and other pairings are untested. A dependency ` +
        `bot usually causes this by updating vite-plus and the \`vite\` alias in separate ` +
        `PRs. Update the \`vite\` alias to npm:${CORE_PACKAGE_NAME}@${expectedVersion} ` +
        `where it is declared (pnpm catalog, overrides, or dependencies), or run ` +
        `\`vp migrate\` to realign it. Set ${SKIP_CORE_VERSION_CHECK_ENV}=1 to skip this check.`,
    );
  }
}

/**
 * Orchestrates the guard: skip in preview/override flows where the CLI's
 * version identity is redefined (`VP_VERSION` set, e.g. pkg.pr.new and
 * registry-bridge installs pointing the alias at a tarball URL), skip on the
 * explicit escape hatch, otherwise read the project's `vite` and assert.
 *
 * Exported (with injectable `deps`) for unit testing.
 */
export function checkCoreVersionMatch(
  projectDir: string = process.cwd(),
  deps: {
    createRequire?: (from: string) => { resolve: (id: string) => string };
    readFile?: (file: string) => string;
  } = {},
): void {
  if (process.env[SKIP_CORE_VERSION_CHECK_ENV] || process.env.VP_VERSION) {
    return;
  }
  assertCoreVersionMatch(readProjectVitePackage(projectDir, deps.createRequire, deps.readFile));
}
