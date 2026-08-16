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

import { VITE_PLUS_CORE_PACKAGE_NAME, VITE_PLUS_OVERRIDE_PACKAGES } from './constants.ts';
import { detectPackageMetadata } from './package.ts';

export const SKIP_CORE_VERSION_CHECK_ENV = 'VP_SKIP_CORE_VERSION_CHECK';

/**
 * Extract the exact core version from a `vite` alias spec
 * (`npm:@voidzero-dev/vite-plus-core@<version>`). Returns `null` for every
 * other shape: preview and ecosystem flows redefine the alias to a tarball
 * URL or `file:` spec (via `VP_VERSION` / `VP_OVERRIDE_PACKAGES`), and those
 * carry no exact version to compare against. Deriving the skip from the spec
 * instead of from env-var names keeps the guard active when `VP_VERSION` is
 * merely a plain version (the Rust CLI injects one into every child env, so
 * nested `vp` runs would otherwise silently lose the check).
 *
 * Exported for unit testing.
 */
export function parseCoreAliasVersion(aliasSpec: string | undefined): string | null {
  const prefix = `npm:${VITE_PLUS_CORE_PACKAGE_NAME}@`;
  if (!aliasSpec?.startsWith(prefix)) {
    return null;
  }
  const version = aliasSpec.slice(prefix.length);
  return /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version) ? version : null;
}

/**
 * Throw when the project's aliased core version differs from the version the
 * CLI expects. A no-op when no aliased core is installed.
 *
 * Exported for unit testing.
 */
export function assertCoreVersionMatch(
  installedVersion: string | null | undefined,
  expectedVersion: string,
): void {
  if (installedVersion && installedVersion !== expectedVersion) {
    // Keep every version inside a `@voidzero-dev/vite-plus-core@<x>` context:
    // the PTY snapshot redactor masks the CLI's own version only in that form
    // (a bare `vite-plus@<x>` stays verbatim and would churn every release).
    throw new Error(
      `The project's \`vite\` alias resolves to ${VITE_PLUS_CORE_PACKAGE_NAME}@${installedVersion}, ` +
        `but this vite-plus CLI requires ${VITE_PLUS_CORE_PACKAGE_NAME}@${expectedVersion}: the two ` +
        `packages are published in lockstep and other pairings are untested. A dependency ` +
        `bot usually causes this by updating vite-plus and the \`vite\` alias in separate ` +
        `PRs. Update the \`vite\` alias to npm:${VITE_PLUS_CORE_PACKAGE_NAME}@${expectedVersion} ` +
        `where it is declared (catalog, overrides, resolutions, or dependencies), or run ` +
        `\`vp migrate\` to realign it. Set ${SKIP_CORE_VERSION_CHECK_ENV}=1 to skip this check.`,
    );
  }
}

/**
 * Orchestrates the guard: honor the escape hatch, derive the expected version
 * from the alias spec the CLI itself scaffolds (skipping redefined preview
 * specs), read what `vite` resolves to from the project (the copy plugins and
 * configs import), and assert. A project on real Vite, or with no `vite`
 * installed, passes.
 *
 * The `aliasSpec` parameter exists for unit tests; production callers use the
 * default.
 */
export function checkCoreVersionMatch(
  projectDir: string = process.cwd(),
  aliasSpec: string | undefined = VITE_PLUS_OVERRIDE_PACKAGES.vite,
): void {
  if (process.env[SKIP_CORE_VERSION_CHECK_ENV]) {
    return;
  }
  const expectedVersion = parseCoreAliasVersion(aliasSpec);
  if (!expectedVersion) {
    return;
  }
  const installed = detectPackageMetadata(projectDir, 'vite');
  assertCoreVersionMatch(
    installed && installed.name === VITE_PLUS_CORE_PACKAGE_NAME ? installed.version : null,
    expectedVersion,
  );
}

let coreVersionChecked = false;

/**
 * Memoized wrapper for the resolver path. The `vite`/`test` resolvers run
 * once per intercepted script command, so a `vp run` across a large workspace
 * would repeat the same read of an unchanging file; the project dir never
 * changes within a process, so one check suffices.
 */
export function checkCoreVersionMatchOnce(): void {
  if (coreVersionChecked) {
    return;
  }
  coreVersionChecked = true;
  checkCoreVersionMatch();
}
