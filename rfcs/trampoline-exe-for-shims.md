# RFC: Windows Trampoline `.exe` for Shims

## Status

Implemented

## Summary

Replace Windows `.cmd` wrapper scripts with lightweight trampoline `.exe` binaries for all shim tools (`vp`, `node`, `npm`, `npx`, `corepack`, `vpx`, `vpr`, and globally installed package binaries). This eliminates the `Terminate batch job (Y/N)?` prompt that appears when users press Ctrl+C, providing the same clean signal behavior as direct `.exe` invocation.

## Motivation

### The Problem

On Windows, the vite-plus CLI previously exposed tools through `.cmd` batch file wrappers:

```
~/.vite-plus/bin/
├── vp.cmd          → calls current\bin\vp.exe
├── node.cmd        → calls vp.exe env exec node
├── npm.cmd         → calls vp.exe env exec npm
├── npx.cmd         → calls vp.exe env exec npx
└── ...
```

When a user presses Ctrl+C while a command is running through a `.cmd` wrapper, `cmd.exe` intercepts the signal and displays:

```
Terminate batch job (Y/N)?
```

This is a fundamental limitation of batch file execution on Windows. The prompt:

- Interrupts the normal Ctrl+C workflow that users expect
- May appear multiple times (once per `.cmd` in the chain)
- Differs from Unix behavior where Ctrl+C cleanly terminates the process
- Cannot be suppressed from within the batch file

### Confirmed Behavior

As demonstrated in [issue #835](https://github.com/voidzero-dev/vite-plus/issues/835):

1. Running `vp dev` (through `vp.cmd`) shows `Terminate batch job (Y/N)?` on Ctrl+C
2. Running `~/.vite-plus/current/bin/vp.exe dev` directly does **NOT** show the prompt
3. Running `npm.cmd run dev` shows the prompt; running `npm.ps1 run dev` does not
4. The prompt can appear multiple times when `.cmd` wrappers chain (e.g., `vp.cmd` → `npm.cmd`)

### Why `.ps1` Scripts Are Not Sufficient

PowerShell `.ps1` scripts avoid the Ctrl+C issue but have critical limitations:

- `where.exe` and `which` do not discover `.ps1` files as executables
- Only work in PowerShell, not in `cmd.exe`, Git Bash, or other shells
- Cannot serve as universal shims

## Architecture

### Unix (Symlink-Based — Unchanged)

On Unix, shims are symlinks to the `vp` binary. The binary detects the tool name from `argv[0]`:

```
~/.vite-plus/bin/
├── vp       → ../current/bin/vp     (symlink)
├── node     → ../current/bin/vp     (symlink)
├── npm      → ../current/bin/vp     (symlink)
├── npx      → ../current/bin/vp     (symlink)
├── corepack → ../current/bin/vp     (symlink)
├── vpx      → ../current/bin/vp     (symlink)
└── vpr      → ../current/bin/vp     (symlink)
```

### Windows (Trampoline `.exe` Files)

```
~/.vite-plus/bin/
├── vp.exe       # Trampoline → spawns current\bin\vp.exe
├── node.exe     # Trampoline → sets VP_SHIM_TOOL=node, spawns vp.exe
├── npm.exe      # Trampoline → sets VP_SHIM_TOOL=npm, spawns vp.exe
├── npx.exe      # Trampoline → sets VP_SHIM_TOOL=npx, spawns vp.exe
├── corepack.exe # Trampoline → sets VP_SHIM_TOOL=corepack, spawns vp.exe
├── vpx.exe      # Trampoline → sets VP_SHIM_TOOL=vpx, spawns vp.exe
├── vpr.exe      # Trampoline → sets VP_SHIM_TOOL=vpr, spawns vp.exe
└── tsc.exe      # Trampoline → sets VP_SHIM_TOOL=tsc, spawns vp.exe (package shim)
```

Each trampoline is a copy of `vp-shim.exe` (the template binary distributed alongside `vp.exe`).

**Note**: npm-installed packages (via `npm install -g`) still use `.cmd` wrappers because they lack `PackageMetadata` and need to point directly at npm's generated scripts.

## Implementation

### Crate Structure

```
crates/vp_trampoline/
├── Cargo.toml           # Zero dependencies, own release profile
├── Cargo.lock           # Own lockfile (the crate is not a workspace member)
├── .cargo/
│   └── config.toml      # build-std flags + target-dir = repo-root target/
├── src/
│   ├── main.rs          # Entry points + portable non-Windows fallback
│   ├── win.rs           # Windows implementation: raw Win32, no_main entry
│   └── cmdline.rs       # Pure UTF-16 helpers with cross-platform unit tests
```

The crate is excluded from the workspace (`exclude` in the root `Cargo.toml`).
Two build requirements force this:

- The release profile sets `panic = "immediate-abort"`. Cargo ignores `panic`
  in per-package profile overrides, so the crate needs its own profile.
- The crate-local `.cargo/config.toml` enables build-std. Cargo reads that
  config only when it runs from the crate directory.

Build it from the crate directory:

```bash
cd crates/vp_trampoline && cargo build --release [--target <triple>]
```

Artifacts land in the repo-root `target/` directory (the crate config sets
`target-dir = "../../target"`), so CI steps and `install-global-cli` find
`vp-shim.exe` in the same place as workspace-built binaries. The build needs
the pinned nightly toolchain and the `rust-src` component; both come from the
repo `rust-toolchain.toml`.

### Trampoline Binary

The trampoline has **zero external dependencies**: all Win32 calls are raw
`extern "system"` declarations against KERNEL32, so the heavy
`windows`/`windows-core` crates never enter the build. It also never touches
`core::fmt`; diagnostics go through `WriteFile` with a hand-rolled decimal
formatter.

On Windows the binary is `#![no_main]` with an exported `mainCRTStartup`
symbol, so neither the CRT startup nor `std` runtime init runs. The flow in
`src/win.rs`:

1. `GetModuleFileNameW` gives our own path. The filename stem is the tool
   name; the grandparent directory is `VP_HOME`.
2. `SetEnvironmentVariableW` sets `VP_HOME` (and `VP_SHIM_TOOL`, and removes
   `VP_TOOL_RECURSION`, when the tool is not `vp`) on our own environment;
   the child inherits it.
3. The child command line is `"<VP_HOME>\current\bin\vp.exe"` plus the raw
   tail of `GetCommandLineW` after the first (program) argument, forwarded
   byte for byte. The skip follows the MSVC rule for the program name:
   quotes toggle, backslashes do not escape. This preserves the caller's
   exact quoting, which `std::process::Command`'s re-quoting cannot
   guarantee.
4. `SetConsoleCtrlHandler` installs a handler that ignores Ctrl+C and
   Ctrl+Break; the child decides how to react.
5. `CreateProcessW` spawns the child with inherited handles and our startup
   info. When the parent redirected stdio (`STARTF_USESTDHANDLES`), the std
   handles are forced inheritable first, the same way uv-trampoline and
   distlib's launcher do.
6. `WaitForSingleObject` + `GetExitCodeProcess` + `ExitProcess` propagate the
   child's exit code unchanged.

Every failure path reports the failed call, the path involved, and the
`GetLastError` code to stderr before it exits. A missing `vp.exe` additionally
prints a recovery hint (reinstall or `vp env setup`).

The non-Windows build keeps the previous portable `std::process::Command`
implementation in `main.rs`. It exists so the crate builds and tests on every
platform; Unix shims are symlinks and never use it.

### Size Optimization

| Technique                                                              | Status |
| ---------------------------------------------------------------------- | ------ |
| Zero external dependencies (raw FFI, no `windows` crate)               | Done   |
| No `core::fmt` (diagnostics via `WriteFile` + manual decimal formatter) | Done   |
| Own profile: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip`  | Done   |
| build-std: recompile `std` with this profile (`-Zbuild-std`)           | Done   |
| `panic = "immediate-abort"` (no panic formatting, unwinding, backtrace) | Done   |
| `#![no_main]` + `mainCRTStartup` (no CRT startup, no `std` runtime init) | Done |
| Raw `CreateProcessW` instead of `std::process::Command`                | Done   |

**Binary size**: 8,192 B on x86_64-pc-windows-msvc (9,216 B on aarch64), with
full error diagnostics. The same source built against the precompiled `std`
as a plain workspace member was ~208KB, and the build-std profile alone (with
`std::process::Command` still in place) gives ~72KB; see Future Optimizations
for the full measured ladder. The exe imports only KERNEL32.

### Environment Variables

The trampoline sets three env vars before spawning `vp.exe`:

| Variable            | When                       | Purpose                                                                        |
| ------------------- | -------------------------- | ------------------------------------------------------------------------------ |
| `VP_HOME`           | Always                     | Tells vp.exe the install directory (derived from `bin_dir.parent()`)           |
| `VP_SHIM_TOOL`      | Tool shims only (not "vp") | Tells vp.exe to enter shim dispatch mode for the named tool                    |
| `VP_TOOL_RECURSION` | Removed for tool shims     | Clears the recursion marker for fresh version resolution in nested invocations |

### Ctrl+C Handling

The trampoline installs a console control handler that returns `TRUE` (1):

1. When Ctrl+C is pressed, Windows sends `CTRL_C_EVENT` to **all processes** in the console group
2. The trampoline's handler returns 1 (TRUE) → trampoline stays alive
3. The child process (`vp.exe` → Node.js) receives the **same** event
4. The child decides how to handle it (typically exits gracefully)
5. The trampoline detects the child's exit and propagates its exit code

**No "Terminate batch job?" prompt** because there is no batch file involved.

### Integration with Shim Detection

`detect_shim_tool()` in `shim/mod.rs` checks `VP_SHIM_TOOL` env var **before** `argv[0]`:

```
Trampoline (node.exe)
  → sets VP_SHIM_TOOL=node, VP_HOME=..., removes VP_TOOL_RECURSION
  → spawns current/bin/vp.exe with original args
    → detect_shim_tool() reads env var → "node"
    → dispatch("node", args)
    → resolves Node.js version, executes real node
```

### Running Exe Overwrite

When `vp env setup --refresh` is invoked through the trampoline (`~/.vite-plus/bin/vp.exe`), the trampoline is still running. Windows prevents overwriting a running `.exe`. The solution:

1. Rename existing `vp.exe` to `vp.exe.<unix_timestamp>.old`
2. Copy new trampoline to `vp.exe`
3. Best-effort cleanup of all `*.old` files in the bin directory

### Upgrade Refresh

During `vp upgrade`, after the `current` link is swapped to the new version, `vp env setup --refresh` is invoked to regenerate all trampoline `.exe` files. This ensures that when the trampoline binary (`vp-shim.exe`) changes between versions, all shims pick up the new version:

1. **Core shims** (`vp.exe`, `node.exe`, `npm.exe`, `npx.exe`, `corepack.exe`, `vpx.exe`, `vpr.exe`) are refreshed by the standard `--refresh` logic.
2. **Package shims** (e.g., `tsc.exe`, `eslint.exe`, installed via `vp install -g`) are discovered by scanning `~/.vite-plus/bins/` for `BinConfig` entries with `source: Vp`, and each `.exe` is replaced with the new trampoline.

Package shims installed via npm interception (`source: Npm`) use `.cmd` wrappers, not trampoline `.exe` files, and are not affected by this refresh.

Additionally, re-installing a global package (`vp install -g <pkg>`) always re-copies the current trampoline, ensuring the shim stays up to date even without a full upgrade.

### Distribution

The trampoline binary (`vp-shim.exe`) is distributed alongside `vp.exe`:

```
~/.vite-plus/current/bin/
├── vp.exe          # Main CLI binary
└── vp-shim.exe     # Trampoline template (copied as shims)
```

Included in:

- Platform npm packages (`@voidzero-dev/vite-plus-cli-win32-x64-msvc`)
- Release artifacts (`.github/workflows/release.yml`)
- `install.ps1` and `install.sh` (both local dev and download paths)
- `extract_platform_package()` in the upgrade path

### Legacy Fallback

When installing a pre-trampoline version (no `vp-shim.exe` in the package):

- `install.ps1` falls back to creating `.cmd` + shell script wrappers
- Stale trampoline `.exe` shims from a newer install are removed (`.exe` takes precedence over `.cmd` on Windows PATH)

## Comparison with uv-trampoline

| Aspect              | uv-trampoline                            | vite-plus trampoline                 |
| ------------------- | ---------------------------------------- | ------------------------------------ |
| **Purpose**         | Launch Python with embedded script       | Forward to `vp.exe`                  |
| **Complexity**      | High (PE resources, zipimport)           | Low (filename + spawn)               |
| **Data embedding**  | PE resources (kind, path, script ZIP)    | None (uses filename + relative path) |
| **Dependencies**    | `windows` crate (unsafe, no CRT)         | Zero (raw FFI declaration)           |
| **Toolchain**       | Nightly Rust (`panic="immediate-abort"`) | Nightly Rust (same technique)        |
| **Binary size**     | 39-47 KB                                 | ~8 KB                                |
| **Entry point**     | `#![no_main]` + `mainCRTStartup`         | Same approach                        |
| **Error output**    | `ufmt` (no `core::fmt`)                  | `WriteFile` + Win32 error codes      |
| **Ctrl+C handling** | `SetConsoleCtrlHandler` → ignore         | Same approach                        |
| **Exit code**       | `GetExitCodeProcess` → `exit()`          | Same approach                        |

The vite-plus trampoline is smaller because it embeds no data in PE resources and needs no path canonicalization, job objects, or GUI subsystem support: it reads its own filename, finds `vp.exe` at a fixed relative path, and spawns it. Both projects share the same build recipe and entry-point structure.

## Alternatives Considered

### 1. NTFS Hardlinks (Rejected)

Hardlinks resolve to physical file inodes, not through directory junctions. After `vp` upgrade re-points `current`, hardlinks in `bin/` still reference the old binary.

### 2. Windows Symbolic Links (Rejected)

Requires administrator privileges or Developer Mode. Not reliable for all users.

### 3. PowerShell `.ps1` Scripts (Rejected)

`where.exe` and `which` do not find `.ps1` files. Only works in PowerShell.

### 4. Copy `vp.exe` as Each Shim (Rejected)

~5-10MB per copy. Trampoline achieves the same result at ~8KB.

### 5. `windows` Crate for FFI (Rejected)

Adds ~100KB to the binary for a single `SetConsoleCtrlHandler` call. Raw FFI declaration is sufficient.

## Future Optimizations

Every variant below was built with cargo-xwin and measured on
x86_64-pc-windows-msvc. The ladder shows what each technique buys and serves
as reference material for future size work.

| Variant                                                                    | Toolchain | Size      |
| -------------------------------------------------------------------------- | --------- | --------- |
| `std::process::Command` source, precompiled `std`, `opt-level="z"` + fat LTO + `panic="abort"` | stable | 212,992 B |
| Same source + build-std + `panic="immediate-abort"`                        | nightly   | 73,728 B  |
| Same + `#![no_main]` + `mainCRTStartup` + `atexit` stub                    | nightly   | 69,632 B  |
| Raw Win32 rewrite, normal `main`, stable, no build-std                     | stable    | 105,984 B |
| Raw Win32 rewrite, normal `main` + build-std                               | nightly   | 13,824 B  |
| Raw Win32 rewrite + `#![no_main]`, no diagnostics                          | nightly   | 6,656 B   |
| Raw Win32 rewrite + `#![no_main]` + full error diagnostics (shipped)       | nightly   | 8,192 B   |

For comparison: uv-trampoline ships 45,056 B (x64 console), Scoop's default
kiennq shim is 136,192 B (statically linked MSVC C), and Scoop once vendored
and then reverted a 317,952 B Rust shim.

### Gotchas (all hit while measuring)

1. **`atexit` link failure**: current nightlies register TLS destructor
   cleanup through C `atexit`. Under `#![no_main]` that symbol pulls
   `msvcrt.lib(utility.obj)`, and the link fails with undefined `__vcrt_*` /
   `__acrt_*` CRT init internals. Fix: export a no-op
   `extern "C" fn atexit(...) -> i32 { 0 }` (see win.rs). The trampoline
   never needs exit-time TLS destructors. uv's documented
   `rustc-link-lib=ucrt` workaround (rust-lang/rust#143172) does not fix this
   pull; uv's pinned older nightly simply predates the `atexit` registration.
2. **Subsystem**: `#![no_main]` requires an explicit
   `#![windows_subsystem = "console"]`, or lld fails with "subsystem must be
   defined".
3. **Do not use `+crt-static`**: it links the static CRT and grows the binary
   to ~115KB.
4. **Dev profile**: at `opt-level = 0` the compiler can emit references to
   the MSVC unwinding helper `__CxxFrameHandler3` even with
   `panic = "immediate-abort"`, and the link fails. Keep `opt-level = 1` and
   LTO in the dev profile (uv does the same).

### Remaining options

- Assign the child to a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
  (as uv does), so a killed shim also kills its child. Costs a few KB.
- Commit prebuilt, reproducible trampoline binaries (uv checks in
  `/Brepro`-normalized exes and verifies them byte for byte in CI) to
  decouple the shim from toolchain drift.

## References

- [Issue #835](https://github.com/voidzero-dev/vite-plus/issues/835): Original feature request with video reproduction
- [uv-trampoline](https://github.com/astral-sh/uv/tree/main/crates/uv-trampoline): Reference implementation by astral-sh. Same build recipe (workspace exclusion, build-std, `panic="immediate-abort"`, cargo-xwin), plus `#![no_main]`, raw Win32, and a CI `cargo bloat` gate that rejects any `core::fmt`/`std::panicking` symbol.
- [Scoop shims](https://github.com/ScoopInstaller/Scoop/tree/master/supporting/shims): vendored native C shim (136KB, from kiennq/scoop-better-shimexe) and C# .NET shim (9.7KB); launch targets come from a sibling `.shim` text file.
- [RFC: env-command](./env-command.md): Shim architecture documentation
- [RFC: upgrade-command](./upgrade-command.md): Upgrade/rollback flow
