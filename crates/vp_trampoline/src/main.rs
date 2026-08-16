//! Minimal Windows trampoline for vite-plus shims.
//!
//! This binary is copied and renamed for each shim tool (node.exe, npm.exe, etc.).
//! It detects the tool name from its own filename, then spawns `vp.exe` with the
//! `VP_SHIM_TOOL` environment variable set, allowing `vp.exe` to enter
//! shim dispatch mode.
//!
//! On Ctrl+C, the trampoline ignores the signal (the child process handles it),
//! avoiding the "Terminate batch job (Y/N)?" prompt that `.cmd` wrappers produce.
//!
//! **Size optimization**: On Windows the binary uses `#![no_main]` with a
//! `mainCRTStartup` entry point and raw Win32 calls instead of
//! `std::process::Command`, in the uv-trampoline structure. Together with the
//! build-std + `panic = "immediate-abort"` profile (see Cargo.toml) this keeps
//! the exe at ~8KB. Error paths still report the failed call, the path
//! involved, and the Windows error code. See rfcs/trampoline-exe-for-shims.md.
//!
//! The non-Windows build keeps the portable `std::process::Command`
//! implementation; it exists so the crate builds and tests everywhere, and
//! never ships (Unix shims are symlinks).
//!
//! See: <https://github.com/voidzero-dev/vite-plus/issues/835>

#![cfg_attr(windows, no_main)]
#![cfg_attr(windows, windows_subsystem = "console")]

#[cfg_attr(not(windows), allow(dead_code))]
mod cmdline;
#[cfg(windows)]
mod win;

/// The linker picks this symbol as the console-subsystem entry point, so no
/// `/ENTRY:` flag is needed. `std` runtime init never runs; see win.rs.
#[cfg(windows)]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn mainCRTStartup() -> ! {
    win::run()
}

/// Preserve Unix signal termination using the shell's `128 + signal` convention.
#[cfg(not(windows))]
fn exit_code_from_status(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}

#[cfg(not(windows))]
fn main() {
    use std::{
        env,
        process::{self, Command},
    };

    // 1. Determine tool name from our own executable filename
    let exe_path = env::current_exe().unwrap_or_else(|_| process::exit(1));
    let tool_name =
        exe_path.file_stem().and_then(|s| s.to_str()).unwrap_or_else(|| process::exit(1));

    // 2. Locate vp.exe: <bin_dir>/../current/bin/vp.exe
    let bin_dir = exe_path.parent().unwrap_or_else(|| process::exit(1));
    let vp_home = bin_dir.parent().unwrap_or_else(|| process::exit(1));
    let vp_exe = vp_home.join("current").join("bin").join("vp.exe");

    // 3. Spawn vp.exe
    //    - Always set VP_HOME so vp.exe uses the correct home directory
    //    - If tool is "vp", run in normal CLI mode (no VP_SHIM_TOOL)
    //    - Otherwise, set VP_SHIM_TOOL so vp.exe enters shim dispatch
    let mut cmd = Command::new(&vp_exe);
    cmd.args(env::args_os().skip(1));
    cmd.env("VP_HOME", vp_home);

    if tool_name != "vp" {
        cmd.env("VP_SHIM_TOOL", tool_name);
        // Clear the recursion marker so nested shim invocations (e.g., npm
        // spawning node) get fresh version resolution instead of falling
        // through to passthrough mode.
        // Must match vp_shared::env_vars::VP_TOOL_RECURSION
        cmd.env_remove("VP_TOOL_RECURSION");
    }

    // 4. Execute and propagate exit code.
    match cmd.status() {
        Ok(status) => process::exit(exit_code_from_status(status)),
        Err(_) => {
            use std::io::Write;
            let stderr = std::io::stderr();
            let mut handle = stderr.lock();
            let _ = handle.write_all(b"vite-plus: failed to execute ");
            let _ = handle.write_all(vp_exe.as_os_str().as_encoded_bytes());
            let _ = handle.write_all(b"\n");
            process::exit(1);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn preserves_signal_exit_code() {
        let status = Command::new("/bin/sh").arg("-c").arg("kill -ILL $$").status().unwrap();
        assert_eq!(exit_code_from_status(status), 132);
    }
}
