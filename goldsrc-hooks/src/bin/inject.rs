//! Minimal manual DLL injector for testing goldsrc-hooks against a running
//! hl.exe, independent of any future DoD Studio capture-pipeline wiring.
//!
//! Usage: inject <pid> <path-to-dll>
//!
//! Standard technique: allocate a small buffer in the target process for the
//! DLL path, write the path into it, then start a remote thread whose entry
//! point is kernel32!LoadLibraryA with that buffer as its argument -- the
//! same effect as the target process calling LoadLibraryA itself.

use std::env;
use std::ffi::c_void;
use std::process::ExitCode;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::Memory::{VirtualAllocEx, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
use windows_sys::Win32::System::Threading::{
    CreateRemoteThread, OpenProcess, WaitForSingleObject, INFINITE, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let [_, pid_arg, dll_path] = args.as_slice() else {
        eprintln!("usage: inject <pid> <path-to-dll>");
        return ExitCode::FAILURE;
    };

    let Ok(pid) = pid_arg.parse::<u32>() else {
        eprintln!("'{pid_arg}' is not a valid process id");
        return ExitCode::FAILURE;
    };

    let dll_path_abs = match std::fs::canonicalize(dll_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("could not resolve '{dll_path}': {e}");
            return ExitCode::FAILURE;
        }
    };
    // canonicalize() on Windows yields a \\?\-prefixed path; LoadLibraryA
    // handles that fine, but strip it for a friendlier printed path.
    let dll_path_str = dll_path_abs.to_string_lossy().replace("\\\\?\\", "");
    let mut dll_path_c: Vec<u8> = dll_path_str.as_bytes().to_vec();
    dll_path_c.push(0);

    unsafe {
        let process: HANDLE = OpenProcess(
            PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ,
            0,
            pid,
        );
        if process.is_null() {
            eprintln!("OpenProcess({pid}) failed -- is that PID correct, and are you running as the same user (or elevated)?");
            return ExitCode::FAILURE;
        }

        let remote_buf = VirtualAllocEx(
            process,
            std::ptr::null(),
            dll_path_c.len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote_buf.is_null() {
            eprintln!("VirtualAllocEx failed");
            CloseHandle(process);
            return ExitCode::FAILURE;
        }

        let mut written = 0usize;
        let ok = WriteProcessMemory(
            process,
            remote_buf,
            dll_path_c.as_ptr() as *const c_void,
            dll_path_c.len(),
            &mut written,
        );
        if ok == 0 || written != dll_path_c.len() {
            eprintln!("WriteProcessMemory failed (wrote {written}/{} bytes)", dll_path_c.len());
            CloseHandle(process);
            return ExitCode::FAILURE;
        }

        let kernel32 = GetModuleHandleA(c"kernel32.dll".as_ptr() as *const u8);
        if kernel32.is_null() {
            eprintln!("could not get a handle to our own kernel32.dll (should never happen)");
            CloseHandle(process);
            return ExitCode::FAILURE;
        }
        let load_library_a = GetProcAddress(kernel32, c"LoadLibraryA".as_ptr() as *const u8);
        let Some(load_library_a) = load_library_a else {
            eprintln!("could not resolve LoadLibraryA (should never happen)");
            CloseHandle(process);
            return ExitCode::FAILURE;
        };

        // kernel32.dll is loaded at the same address in every process on a
        // given Windows session (ASLR notwithstanding, it's still mapped
        // system-wide from the same base for a given boot), so this address,
        // taken from our own process, is valid to use as the remote thread's
        // start address in the target process too -- the standard technique.
        let load_library_a_thread_start: unsafe extern "system" fn(*mut c_void) -> u32 =
            std::mem::transmute(load_library_a);
        let thread = CreateRemoteThread(
            process,
            std::ptr::null(),
            0,
            Some(load_library_a_thread_start),
            remote_buf,
            0,
            std::ptr::null_mut(),
        );
        if thread.is_null() {
            eprintln!("CreateRemoteThread failed");
            CloseHandle(process);
            return ExitCode::FAILURE;
        }

        WaitForSingleObject(thread, INFINITE);
        CloseHandle(thread);
        CloseHandle(process);
    }

    println!("Injected {dll_path_str} into process {pid}.");
    println!("Check %APPDATA%\\dod-studio\\logs\\dodstudio_goldsrc_hooks.log for its own diagnostics.");
    ExitCode::SUCCESS
}
