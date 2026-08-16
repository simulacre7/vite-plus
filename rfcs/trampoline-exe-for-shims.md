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
│   └── main.rs          # ~90 lines, single-file binary
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

The trampoline has **zero external dependencies** — the Win32 FFI call (`SetConsoleCtrlHandler`) is declared inline to avoid the heavy `windows`/`windows-core` crates. It also avoids `core::fmt` (~100KB overhead) by never using `format!`, `eprintln!`, `println!`, or `.unwrap()`.

```rust
use std::{env, process::{self, Command}};

fn main() {
    // 1. Determine tool name from own filename (e.g., node.exe → "node")
    let exe_path = env::current_exe().unwrap_or_else(|_| process::exit(1));
    let tool_name = exe_path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| process::exit(1));

    // 2. Locate vp.exe at ../current/bin/vp.exe
    let bin_dir = exe_path.parent().unwrap_or_else(|| process::exit(1));
    let vp_home = bin_dir.parent().unwrap_or_else(|| process::exit(1));
    let vp_exe = vp_home.join("current").join("bin").join("vp.exe");

    // 3. Install Ctrl+C handler (ignores signal; child handles it)
    install_ctrl_handler();

    // 4. Spawn vp.exe with env vars
    let mut cmd = Command::new(&vp_exe);
    cmd.args(env::args_os().skip(1));
    cmd.env("VP_HOME", vp_home);

    if tool_name != "vp" {
        cmd.env("VP_SHIM_TOOL", tool_name);
        cmd.env_remove("VP_TOOL_RECURSION");
    }

    // 5. Propagate exit code (error message via write_all, not eprintln!)
    match cmd.status() {
        Ok(status) => process::exit(exit_code_from_status(status)),
        Err(_) => {
            use std::io::Write;
            let mut stderr = std::io::stderr().lock();
            let _ = stderr.write_all(b"vite-plus: failed to execute ");
            let _ = stderr.write_all(vp_exe.as_os_str().as_encoded_bytes());
            let _ = stderr.write_all(b"\n");
            process::exit(1);
        }
    }
}

fn install_ctrl_handler() {
    type HandlerRoutine = unsafe extern "system" fn(ctrl_type: u32) -> i32;
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
    }
    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 { 1 }
    unsafe { SetConsoleCtrlHandler(Some(handler), 1); }
}
```

### Size Optimization

| Technique                                                              | Status |
| ---------------------------------------------------------------------- | ------ |
| Zero external dependencies (raw FFI, no `windows` crate)               | Done   |
| No direct `core::fmt` usage (avoid `eprintln!`/`format!`/`.unwrap()`)  | Done   |
| Own profile: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip`  | Done   |
| build-std: recompile `std` with this profile (`-Zbuild-std`)           | Done   |
| `panic = "immediate-abort"` (no panic formatting, unwinding, backtrace) | Done   |
| `build-std-features = ["optimize_for_size"]` (drops panic-unwind, backtrace features) | Done |

**Binary size**: ~72KB on x86_64-pc-windows-msvc (~70KB on aarch64). With the
precompiled `std` the same source built to ~208KB: the prebuilt rlib carries
`lang_start` init, panic formatting, and backtrace support, and `opt-level`
cannot remove code that a prebuilt rlib already contains. build-std recompiles
`std` under this crate's own profile, and `panic = "immediate-abort"` compiles
the panic machinery out. A further reduction to ~7KB is measured and documented
under Future Optimizations.

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
| **Binary size**     | 39-47 KB                                 | ~72 KB                               |
| **Entry point**     | `#![no_main]` + `mainCRTStartup`         | Standard `fn main()`                 |
| **Error output**    | `ufmt` (no `core::fmt`)                  | `write_all` (no `core::fmt`)         |
| **Ctrl+C handling** | `SetConsoleCtrlHandler` → ignore         | Same approach                        |
| **Exit code**       | `GetExitCodeProcess` → `exit()`          | `Command::status()` → `exit()`       |

The vite-plus trampoline is simpler because it embeds no data in PE resources: it reads its own filename, finds `vp.exe` at a fixed relative path, and spawns it. Both projects build with build-std and `panic = "immediate-abort"`. The remaining ~27KB gap comes from `std::process::Command` and `std` runtime init, which uv replaces with raw `CreateProcessA` and `#![no_main]` (see Future Optimizations for the measured equivalent here).

## Alternatives Considered

### 1. NTFS Hardlinks (Rejected)

Hardlinks resolve to physical file inodes, not through directory junctions. After `vp` upgrade re-points `current`, hardlinks in `bin/` still reference the old binary.

### 2. Windows Symbolic Links (Rejected)

Requires administrator privileges or Developer Mode. Not reliable for all users.

### 3. PowerShell `.ps1` Scripts (Rejected)

`where.exe` and `which` do not find `.ps1` files. Only works in PowerShell.

### 4. Copy `vp.exe` as Each Shim (Rejected)

~5-10MB per copy. Trampoline achieves the same result at ~72KB.

### 5. `windows` Crate for FFI (Rejected)

Adds ~100KB to the binary for a single `SetConsoleCtrlHandler` call. Raw FFI declaration is sufficient.

## Future Optimizations

Every variant below was built with cargo-xwin and measured on
x86_64-pc-windows-msvc. The numbers serve as reference material for further
size work.

| Variant                                                                    | Toolchain | Size      |
| -------------------------------------------------------------------------- | --------- | --------- |
| Current source, precompiled `std`, `opt-level="z"` + fat LTO + `panic="abort"` | stable    | 212,992 B |
| Current source + build-std + `panic="immediate-abort"` (shipped today)     | nightly   | 73,728 B  |
| Same + `#![no_main]` + `mainCRTStartup` + `atexit` stub                    | nightly   | 69,632 B  |
| Raw Win32 rewrite, normal `main`, stable, no build-std                     | stable    | 105,984 B |
| Raw Win32 rewrite, normal `main` + build-std                               | nightly   | 13,824 B  |
| Raw Win32 rewrite + `#![no_main]` (uv-trampoline structure)                | nightly   | 6,656 B   |

For comparison: uv-trampoline ships 45,056 B (x64 console), Scoop's default
kiennq shim is 136,192 B (statically linked MSVC C), and Scoop once vendored
and then reverted a 317,952 B Rust shim.

### The 7KB variant

The floor is a raw Win32 rewrite in the uv-trampoline structure. It keeps the
behavior contract of this RFC and produces a 6,656 B exe (7,168 B on aarch64)
that imports only KERNEL32:

- `#![no_main]` plus an exported `mainCRTStartup` symbol. The linker picks
  that symbol as the console-subsystem entry point, so no `/ENTRY:` flag is
  needed. `std` runtime init never runs. Requires
  `build-std-features = ["compiler-builtins-mem"]` so `memcpy`/`memset` come
  from compiler_builtins instead of the CRT.
- Replace `std::process::Command` with `CreateProcessW`. Build the child
  command line as `"<vp_exe>"` plus the raw tail of `GetCommandLineW` after
  the first (program) argument. The skip uses the MSVC rule for the program
  name: quotes toggle, no backslash escapes. This forwards the caller's
  quoting byte for byte, which `Command`'s re-quoting cannot guarantee.
- Set `VP_HOME` / `VP_SHIM_TOOL` with `SetEnvironmentVariableW` on our own
  environment before the spawn; the child inherits it. Remove
  `VP_TOOL_RECURSION` by passing a null value.
- Wait with `WaitForSingleObject`, then propagate the raw child exit code via
  `GetExitCodeProcess` + `ExitProcess`.
- Heap use stays on `Vec` (the `std` System allocator is `HeapAlloc` on the
  process heap; no custom allocator needed).

### Gotchas (all hit while measuring)

1. **`atexit` link failure**: current nightlies register TLS destructor
   cleanup through C `atexit`. Under `#![no_main]` that symbol pulls
   `msvcrt.lib(utility.obj)`, and the link fails with undefined `__vcrt_*` /
   `__acrt_*` CRT init internals. Fix: export a no-op
   `extern "C" fn atexit(...) -> i32 { 0 }`. The trampoline never needs
   exit-time TLS destructors. uv's documented `rustc-link-lib=ucrt`
   workaround (rust-lang/rust#143172) does not fix this pull; uv's pinned
   older nightly simply predates the `atexit` registration.
2. **Subsystem**: `#![no_main]` requires an explicit
   `#![windows_subsystem = "console"]`, or lld fails with "subsystem must be
   defined".
3. **Do not use `+crt-static`**: it links the static CRT and grows the binary
   to ~115KB.
4. **Dev profile**: at `opt-level = 0` the compiler can emit references to
   the MSVC unwinding helper `__CxxFrameHandler3` even with
   `panic = "immediate-abort"`, and the link fails. Keep `opt-level = 1` and
   LTO in the dev profile (uv does the same).

### Open items before adopting the 7KB variant

- Force `HANDLE_FLAG_INHERIT` on the std handles when the parent redirects
  stdio (uv does this before `CreateProcess`); verify parity with
  `std::process::Command` behavior on the Windows PTY snapshot suite.
- Decide whether to assign the child to a job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so a killed shim also kills its
  child. Today neither the shipped trampoline nor the prototype does this.
- Consider committing prebuilt, reproducible trampoline binaries (uv checks
  in `/Brepro`-normalized exes and verifies them byte for byte in CI) to
  decouple the shim from toolchain drift.

## References

- [Issue #835](https://github.com/voidzero-dev/vite-plus/issues/835): Original feature request with video reproduction
- [uv-trampoline](https://github.com/astral-sh/uv/tree/main/crates/uv-trampoline): Reference implementation by astral-sh. Same build recipe (workspace exclusion, build-std, `panic="immediate-abort"`, cargo-xwin), plus `#![no_main]`, raw Win32, and a CI `cargo bloat` gate that rejects any `core::fmt`/`std::panicking` symbol.
- [Scoop shims](https://github.com/ScoopInstaller/Scoop/tree/master/supporting/shims): vendored native C shim (136KB, from kiennq/scoop-better-shimexe) and C# .NET shim (9.7KB); launch targets come from a sibling `.shim` text file.
- [RFC: env-command](./env-command.md): Shim architecture documentation
- [RFC: upgrade-command](./upgrade-command.md): Upgrade/rollback flow
