//! Windows implementation: raw Win32 calls, no `std` runtime init, no
//! `std::process::Command`.
//!
//! The `#![no_main]` entry point in main.rs jumps straight here, so `std`
//! never initializes args, stdio, or thread state. Everything this file needs
//! comes from KERNEL32 (plus `Vec`, whose `std` System allocator is
//! `HeapAlloc` on the process heap and needs no init). The child command line
//! is the raw `GetCommandLineW` tail after the program argument, forwarded
//! byte for byte, so the caller's quoting survives exactly.
//!
//! Every failure path reports the failed call, the path involved, and the
//! Windows error code to stderr before it exits.

use core::{ffi::c_void, ptr};

use crate::cmdline;

type Handle = *mut c_void;

const CP_UTF8: u32 = 65001;
const INFINITE: u32 = 0xFFFF_FFFF;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
const WAIT_OBJECT_0: u32 = 0;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_ENVVAR_NOT_FOUND: u32 = 203;

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    cb_reserved2: u16,
    reserved2: *mut u8,
    std_input: Handle,
    std_output: Handle,
    std_error: Handle,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

type HandlerRoutine = unsafe extern "system" fn(ctrl_type: u32) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleFileNameW(module: Handle, filename: *mut u16, size: u32) -> u32;
    fn GetCommandLineW() -> *const u16;
    fn GetLastError() -> u32;
    fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> i32;
    fn GetStartupInfoW(si: *mut StartupInfoW);
    fn SetHandleInformation(object: Handle, mask: u32, flags: u32) -> i32;
    fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *mut c_void,
        thread_attributes: *mut c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *mut c_void,
        current_directory: *const u16,
        startup_info: *const StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
    fn GetStdHandle(std_handle: u32) -> Handle;
    fn WriteFile(
        handle: Handle,
        buffer: *const u8,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WideCharToMultiByte(
        codepage: u32,
        flags: u32,
        wide: *const u16,
        wide_len: i32,
        out: *mut u8,
        out_len: i32,
        default_char: *const u8,
        used_default: *mut i32,
    ) -> i32;
    fn ExitProcess(exit_code: u32) -> !;
}

// std's TLS guard registers exit-time cleanup through C `atexit`, which lives
// in msvcrt.lib(utility.obj) and drags the whole CRT startup machinery into a
// #![no_main] link. The trampoline exits via ExitProcess and never needs
// exit-time TLS destructors, so satisfy the symbol with a no-op that reports
// success.
#[unsafe(no_mangle)]
pub extern "C" fn atexit(_f: Option<unsafe extern "C" fn()>) -> i32 {
    0
}

/// NUL-terminated UTF-16 literal (compile-time, ASCII input only).
macro_rules! w {
    ($s:literal) => {{
        const S: &str = $s;
        const N: usize = S.len();
        const OUT: [u16; N + 1] = {
            let mut out = [0u16; N + 1];
            let bytes = S.as_bytes();
            let mut i = 0;
            while i < N {
                out[i] = bytes[i] as u16;
                i += 1;
            }
            out
        };
        &OUT
    }};
}

// ---------------------------------------------------------------------------
// Diagnostics. All error paths are cold and write to stderr with WriteFile;
// no core::fmt anywhere.
// ---------------------------------------------------------------------------

fn stderr_write(bytes: &[u8]) {
    unsafe {
        let stderr = GetStdHandle(STD_ERROR_HANDLE);
        if !stderr.is_null() {
            let mut written = 0u32;
            WriteFile(
                stderr,
                bytes.as_ptr(),
                bytes.len() as u32,
                &raw mut written,
                ptr::null_mut(),
            );
        }
    }
}

/// Write a UTF-16 slice to stderr as UTF-8 (best effort).
fn stderr_write_wide(wide: &[u16]) {
    if wide.is_empty() {
        return;
    }
    let len = unsafe {
        WideCharToMultiByte(
            CP_UTF8,
            0,
            wide.as_ptr(),
            wide.len() as i32,
            ptr::null_mut(),
            0,
            ptr::null(),
            ptr::null_mut(),
        )
    };
    if len <= 0 {
        stderr_write(b"<path with unconvertible characters>");
        return;
    }
    let mut utf8: Vec<u8> = Vec::with_capacity(len as usize);
    let written = unsafe {
        WideCharToMultiByte(
            CP_UTF8,
            0,
            wide.as_ptr(),
            wide.len() as i32,
            utf8.as_mut_ptr(),
            len,
            ptr::null(),
            ptr::null_mut(),
        )
    };
    if written > 0 {
        unsafe { utf8.set_len(written as usize) };
        stderr_write(&utf8);
    }
}

fn stderr_write_num(value: u32) {
    let mut buf = [0u8; 10];
    stderr_write(cmdline::format_u32(value, &mut buf));
}

/// Report a failed Win32 call: `vite-plus shim: <what> failed (Windows error N)`.
#[cold]
fn report_call_failure(what: &[u8], error: u32) {
    stderr_write(b"vite-plus shim: ");
    stderr_write(what);
    stderr_write(b" failed (Windows error ");
    stderr_write_num(error);
    stderr_write(b")\n");
}

#[cold]
fn fail_call(what: &[u8]) -> ! {
    report_call_failure(what, unsafe { GetLastError() });
    unsafe { ExitProcess(1) }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

unsafe extern "system" fn ignore_ctrl(_ctrl_type: u32) -> i32 {
    // TRUE: signal handled (ignored). The child also receives the event and
    // decides how to react, which avoids the "Terminate batch job (Y/N)?"
    // prompt the old .cmd wrappers produced.
    1
}

/// Our own module path (no NUL), growing the buffer for long paths.
fn module_path() -> Vec<u16> {
    let mut buf: Vec<u16> = Vec::with_capacity(512);
    loop {
        let cap = buf.capacity();
        let len = unsafe { GetModuleFileNameW(ptr::null_mut(), buf.as_mut_ptr(), cap as u32) };
        if len == 0 {
            fail_call(b"GetModuleFileNameW");
        }
        if (len as usize) < cap {
            unsafe { buf.set_len(len as usize) };
            return buf;
        }
        buf.reserve(cap * 2);
    }
}

/// Set (or, with `None`, remove) an environment variable on our own
/// environment; the child inherits it.
fn set_env(name: &[u16], name_ascii: &[u8], value: Option<&[u16]>) {
    let value_ptr = match value {
        Some(v) => v.as_ptr(),
        None => ptr::null(),
    };
    let ok = unsafe { SetEnvironmentVariableW(name.as_ptr(), value_ptr) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        // Removing a variable that is not set reports ERROR_ENVVAR_NOT_FOUND.
        if value.is_none() && error == ERROR_ENVVAR_NOT_FOUND {
            return;
        }
        stderr_write(b"vite-plus shim: SetEnvironmentVariableW(");
        stderr_write(name_ascii);
        stderr_write(b") failed (Windows error ");
        stderr_write_num(error);
        stderr_write(b")\n");
        unsafe { ExitProcess(1) }
    }
}

fn nul_terminated(slice: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(slice.len() + 1);
    out.extend_from_slice(slice);
    out.push(0);
    out
}

pub fn run() -> ! {
    let sep = b'\\' as u16;
    let alt_sep = b'/' as u16;

    // 1. Own path -> tool name plus the bin and VP_HOME directories.
    let exe = module_path();
    let Some(last_sep) = exe.iter().rposition(|&c| c == sep || c == alt_sep) else {
        stderr_write(b"vite-plus shim: cannot resolve the shim directory from \"");
        stderr_write_wide(&exe);
        stderr_write(b"\"\n");
        unsafe { ExitProcess(1) }
    };
    let file_name = &exe[last_sep + 1..];
    let tool = &file_name[..cmdline::file_stem_len(file_name)];

    // bin_dir = parent of the exe; vp_home = parent of bin_dir.
    let bin_dir = &exe[..last_sep];
    let Some(home_sep) = bin_dir.iter().rposition(|&c| c == sep || c == alt_sep) else {
        stderr_write(b"vite-plus shim: cannot resolve VP_HOME from \"");
        stderr_write_wide(&exe);
        stderr_write(b"\"\n");
        unsafe { ExitProcess(1) }
    };
    let vp_home = &exe[..home_sep];

    // vp_exe = <vp_home>\current\bin\vp.exe (NUL-terminated).
    let suffix = w!("\\current\\bin\\vp.exe");
    let mut vp_exe: Vec<u16> = Vec::with_capacity(vp_home.len() + suffix.len());
    vp_exe.extend_from_slice(vp_home);
    vp_exe.extend_from_slice(suffix);

    // 2. Environment for the child (inherited from our own).
    //    - Always set VP_HOME so vp.exe uses the correct home directory
    //    - If tool is "vp", run in normal CLI mode (no VP_SHIM_TOOL)
    //    - Otherwise, set VP_SHIM_TOOL so vp.exe enters shim dispatch
    set_env(w!("VP_HOME"), b"VP_HOME", Some(&nul_terminated(vp_home)));
    if !cmdline::eq_ascii(tool, b"vp") {
        set_env(w!("VP_SHIM_TOOL"), b"VP_SHIM_TOOL", Some(&nul_terminated(tool)));
        // Clear the recursion marker so nested shim invocations (e.g., npm
        // spawning node) get fresh version resolution instead of falling
        // through to passthrough mode.
        // Must match vp_shared::env_vars::VP_TOOL_RECURSION
        set_env(w!("VP_TOOL_RECURSION"), b"VP_TOOL_RECURSION", None);
    }

    // 3. Child command line: "<vp_exe>" plus the caller's raw argument tail.
    //    Windows paths cannot contain quotes, so wrapping in quotes is enough.
    let tail = unsafe {
        let cl = GetCommandLineW();
        let mut len = 0usize;
        while *cl.add(len) != 0 {
            len += 1;
        }
        let all = core::slice::from_raw_parts(cl, len);
        &all[cmdline::skip_program_argument(all)..]
    };
    let mut child_cmdline: Vec<u16> = Vec::with_capacity(vp_exe.len() + tail.len() + 3);
    child_cmdline.push(b'"' as u16);
    child_cmdline.extend_from_slice(&vp_exe[..vp_exe.len() - 1]);
    child_cmdline.push(b'"' as u16);
    child_cmdline.extend_from_slice(tail);
    child_cmdline.push(0);

    // 4. Ignore Ctrl+C / Ctrl+Break; the child handles them. A failure here
    //    only costs signal cosmetics, so warn and continue.
    let ok = unsafe { SetConsoleCtrlHandler(Some(ignore_ctrl), 1) };
    if ok == 0 {
        report_call_failure(b"warning: SetConsoleCtrlHandler", unsafe { GetLastError() });
    }

    // 5. Spawn vp.exe. Reuse our own startup info; when the parent redirected
    //    stdio (STARTF_USESTDHANDLES), force the handles inheritable so they
    //    reach the child (same as uv-trampoline and distlib's launcher).
    let mut si = unsafe { core::mem::zeroed::<StartupInfoW>() };
    si.cb = size_of::<StartupInfoW>() as u32;
    unsafe { GetStartupInfoW(&raw mut si) };
    if si.flags & STARTF_USESTDHANDLES != 0 {
        for handle in [si.std_input, si.std_output, si.std_error] {
            if !handle.is_null() {
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            }
        }
    }

    let mut pi = ProcessInformation {
        process: ptr::null_mut(),
        thread: ptr::null_mut(),
        process_id: 0,
        thread_id: 0,
    };
    let ok = unsafe {
        CreateProcessW(
            vp_exe.as_ptr(),
            child_cmdline.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            1, // inherit handles
            0,
            ptr::null_mut(), // inherit our (modified) environment
            ptr::null(),
            &raw const si,
            &raw mut pi,
        )
    };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        stderr_write(b"vite-plus: failed to execute \"");
        stderr_write_wide(&vp_exe[..vp_exe.len() - 1]);
        stderr_write(b"\" (Windows error ");
        stderr_write_num(error);
        stderr_write(b")");
        if error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND {
            stderr_write(b": vp.exe is missing; reinstall vite-plus or run `vp env setup`");
        }
        stderr_write(b"\n");
        unsafe { ExitProcess(1) }
    }

    // 6. Wait for the child and propagate its exit code.
    unsafe {
        CloseHandle(pi.thread);
        if WaitForSingleObject(pi.process, INFINITE) != WAIT_OBJECT_0 {
            fail_call(b"WaitForSingleObject");
        }
        let mut code: u32 = 1;
        if GetExitCodeProcess(pi.process, &raw mut code) == 0 {
            fail_call(b"GetExitCodeProcess");
        }
        ExitProcess(code)
    }
}
