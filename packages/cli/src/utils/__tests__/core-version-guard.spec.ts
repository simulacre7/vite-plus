import { afterEach, describe, expect, it, vi } from 'vitest';

import cliPkg from '../../../package.json' with { type: 'json' };
import {
  assertCoreVersionMatch,
  checkCoreVersionMatch,
  CORE_PACKAGE_NAME,
  readProjectVitePackage,
  SKIP_CORE_VERSION_CHECK_ENV,
} from '../core-version-guard.ts';

/**
 * Build injectable deps: `vite/package.json` resolves from any anchor and
 * reads back the given package identity. Mirrors `makeCoverageDeps` in
 * `define-config-plugins.spec.ts`.
 */
function makeViteDeps(pkg: { name: string; version: string } | null): {
  createRequire: (from: string) => { resolve: (id: string) => string };
  readFile: (file: string) => string;
} {
  return {
    createRequire: (_from: string) => ({
      resolve(id: string) {
        if (pkg && id === 'vite/package.json') {
          return '/project/node_modules/vite/package.json';
        }
        throw new Error(`Cannot resolve ${id}`);
      },
    }),
    readFile: () => JSON.stringify(pkg),
  };
}

describe('assertCoreVersionMatch', () => {
  it('does not throw when the aliased core matches the CLI version', () => {
    expect(() =>
      assertCoreVersionMatch({ name: CORE_PACKAGE_NAME, version: '1.2.3' }, '1.2.3'),
    ).not.toThrow();
  });

  it('throws when the aliased core version is skewed from the CLI', () => {
    expect(() =>
      assertCoreVersionMatch({ name: CORE_PACKAGE_NAME, version: '1.2.0' }, '1.2.3'),
    ).toThrow(new RegExp(`npm:${CORE_PACKAGE_NAME}@1\\.2\\.3`));
  });

  it('names both versions and the escape hatch in the error', () => {
    expect(() =>
      assertCoreVersionMatch({ name: CORE_PACKAGE_NAME, version: '1.2.0' }, '1.2.3'),
    ).toThrow(
      expect.objectContaining({
        message: expect.stringMatching(
          new RegExp(
            `${CORE_PACKAGE_NAME}@1\\.2\\.0.*${CORE_PACKAGE_NAME}@1\\.2\\.3.*${SKIP_CORE_VERSION_CHECK_ENV}`,
            's',
          ),
        ),
      }),
    );
  });

  it('does not throw when vite resolves to real Vite instead of the alias', () => {
    expect(() =>
      assertCoreVersionMatch({ name: 'vite', version: '99.0.0' }, '1.2.3'),
    ).not.toThrow();
  });

  it('does not throw when vite is not installed', () => {
    expect(() => assertCoreVersionMatch(null, '1.2.3')).not.toThrow();
  });

  it('does not throw when the resolved package has no version field', () => {
    expect(() => assertCoreVersionMatch({ name: CORE_PACKAGE_NAME }, '1.2.3')).not.toThrow();
  });

  it('defaults the expected version to the CLI package version', () => {
    expect(() =>
      assertCoreVersionMatch({ name: CORE_PACKAGE_NAME, version: cliPkg.version }),
    ).not.toThrow();
    expect(() =>
      assertCoreVersionMatch({ name: CORE_PACKAGE_NAME, version: '0.0.1-never-published' }),
    ).toThrow(new RegExp(`npm:${CORE_PACKAGE_NAME}@`));
  });
});

describe('readProjectVitePackage', () => {
  it('reads the package identity vite resolves to from the project', () => {
    const deps = makeViteDeps({ name: CORE_PACKAGE_NAME, version: '1.2.0' });
    expect(readProjectVitePackage('/project', deps.createRequire, deps.readFile)).toEqual({
      name: CORE_PACKAGE_NAME,
      version: '1.2.0',
    });
  });

  it('returns null when vite is not resolvable', () => {
    const deps = makeViteDeps(null);
    expect(readProjectVitePackage('/project', deps.createRequire, deps.readFile)).toBeNull();
  });

  it('returns null when the resolved package.json is unreadable', () => {
    const deps = makeViteDeps({ name: CORE_PACKAGE_NAME, version: '1.2.0' });
    expect(
      readProjectVitePackage('/project', deps.createRequire, () => {
        throw new Error('EACCES');
      }),
    ).toBeNull();
  });
});

describe('checkCoreVersionMatch', () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('throws for a skewed aliased core', () => {
    vi.stubEnv(SKIP_CORE_VERSION_CHECK_ENV, '');
    vi.stubEnv('VP_VERSION', '');
    const deps = makeViteDeps({ name: CORE_PACKAGE_NAME, version: '0.0.1-never-published' });
    expect(() => checkCoreVersionMatch('/project', deps)).toThrow(
      new RegExp(`${CORE_PACKAGE_NAME}@0\\.0\\.1-never-published`),
    );
  });

  it(`skips the check when ${SKIP_CORE_VERSION_CHECK_ENV} is set`, () => {
    vi.stubEnv(SKIP_CORE_VERSION_CHECK_ENV, '1');
    vi.stubEnv('VP_VERSION', '');
    const deps = makeViteDeps({ name: CORE_PACKAGE_NAME, version: '0.0.1-never-published' });
    expect(() => checkCoreVersionMatch('/project', deps)).not.toThrow();
  });

  it('skips the check when VP_VERSION redefines the CLI version identity', () => {
    vi.stubEnv(SKIP_CORE_VERSION_CHECK_ENV, '');
    vi.stubEnv('VP_VERSION', 'https://pkg.pr.new/voidzero-dev/vite-plus@1891');
    const deps = makeViteDeps({ name: CORE_PACKAGE_NAME, version: '0.0.1-never-published' });
    expect(() => checkCoreVersionMatch('/project', deps)).not.toThrow();
  });

  it('does not throw for a project on real Vite', () => {
    vi.stubEnv(SKIP_CORE_VERSION_CHECK_ENV, '');
    vi.stubEnv('VP_VERSION', '');
    const deps = makeViteDeps({ name: 'vite', version: '99.0.0' });
    expect(() => checkCoreVersionMatch('/project', deps)).not.toThrow();
  });
});
