//! Minimal, dependency-free PE (Portable Executable) helper: walks a loaded
//! module's Import Address Table (IAT) to find a specific imported
//! function's writable slot, so it can be overwritten to intercept that
//! module's calls to it (used for `hw.dll`'s import of `LoadLibraryA`).
//!
//! This is deliberately hand-rolled instead of pulling in a PE-parsing crate:
//! we only need this one narrow operation, and every offset here is defined
//! by the (frozen, decades-stable) Windows PE/COFF format, not by GoldSrc.

use std::ffi::CStr;
use std::os::raw::{c_char, c_void};

#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,
    _reserved: [u16; 29],
    e_lfanew: i32,
}

#[repr(C)]
struct ImageDataDirectory {
    virtual_address: u32,
    size: u32,
}

#[repr(C)]
struct ImageNtHeaders32 {
    signature: u32,
    file_header: ImageFileHeader,
    optional_header: ImageOptionalHeader32,
}

#[repr(C)]
struct ImageFileHeader {
    machine: u16,
    number_of_sections: u16,
    time_date_stamp: u32,
    pointer_to_symbol_table: u32,
    number_of_symbols: u32,
    size_of_optional_header: u16,
    characteristics: u16,
}

#[repr(C)]
struct ImageOptionalHeader32 {
    magic: u16,
    // MajorLinkerVersion..NumberOfRvaAndSizes -- 94 bytes we don't need
    // individual access to, up to DataDirectory[] at offset 96.
    _skip_to_data_dirs: [u8; 94],
    data_directory: [ImageDataDirectory; 16],
}

const IMAGE_DIRECTORY_ENTRY_IMPORT: usize = 1;

#[repr(C)]
struct ImageImportDescriptor {
    original_first_thunk: u32,
    _time_date_stamp: u32,
    _forwarder_chain: u32,
    name: u32,
    first_thunk: u32,
}

#[repr(C)]
struct ImageImportByName {
    _hint: u16,
    name: [c_char; 1],
}

const IMAGE_ORDINAL_FLAG32: u32 = 0x8000_0000;

unsafe fn rva<T>(base: *mut u8, rva: u32) -> *mut T {
    unsafe { base.add(rva as usize) as *mut T }
}

unsafe fn nt_headers(base: *mut u8) -> *mut ImageNtHeaders32 {
    unsafe {
        let dos = base as *const ImageDosHeader;
        rva(base, (*dos).e_lfanew as u32)
    }
}

/// Finds the writable IAT slot (a `*mut *mut c_void`, one pointer-sized cell)
/// that `importing_module` uses to call `import_name` from `from_dll`
/// (case-insensitive DLL name match, e.g. "KERNEL32.dll").
///
/// This is the same technique HLAE itself already uses (see
/// `AfxHookGoldSrc`'s `CAfxImportDllHook`) to intercept a *specific module's*
/// calls to a function, rather than inline-patching the function's own code
/// (which would affect every caller in the process, including the DLL that
/// exports it).
///
/// Safety: `importing_module` must point to a fully-mapped, valid PE image.
pub unsafe fn find_iat_slot(
    importing_module: *mut u8,
    from_dll: &str,
    import_name: &str,
) -> Option<*mut *mut c_void> {
    unsafe {
        let nt = nt_headers(importing_module);
        let dir = &(*nt).optional_header.data_directory[IMAGE_DIRECTORY_ENTRY_IMPORT];
        if dir.virtual_address == 0 {
            return None;
        }

        let mut desc: *mut ImageImportDescriptor = rva(importing_module, dir.virtual_address);
        while (*desc).name != 0 {
            let dll_name_ptr: *const c_char = rva(importing_module, (*desc).name);
            let dll_name = CStr::from_ptr(dll_name_ptr).to_string_lossy();
            if dll_name.eq_ignore_ascii_case(from_dll) {
                // Prefer OriginalFirstThunk (the by-name lookup table) to find
                // *which* slot corresponds to `import_name`; FirstThunk (same
                // index) is the actual IAT the code calls through, and is what
                // we overwrite.
                let lookup_rva = if (*desc).original_first_thunk != 0 {
                    (*desc).original_first_thunk
                } else {
                    (*desc).first_thunk
                };
                let mut lookup: *mut u32 = rva(importing_module, lookup_rva);
                let mut iat: *mut *mut c_void = rva(importing_module, (*desc).first_thunk);

                while *lookup != 0 {
                    if (*lookup & IMAGE_ORDINAL_FLAG32) == 0 {
                        let by_name: *const ImageImportByName = rva(importing_module, *lookup);
                        let name_ptr = (*by_name).name.as_ptr();
                        let candidate = CStr::from_ptr(name_ptr).to_string_lossy();
                        if candidate == import_name {
                            return Some(iat);
                        }
                    }
                    lookup = lookup.add(1);
                    iat = iat.add(1);
                }
                return None;
            }
            desc = desc.add(1);
        }
        None
    }
}

/// One section header, as the PE/COFF spec fixes it (40 bytes).
#[repr(C)]
struct ImageSectionHeader {
    _name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    _size_of_raw_data: u32,
    _pointer_to_raw_data: u32,
    _pointer_to_relocations: u32,
    _pointer_to_linenumbers: u32,
    _number_of_relocations: u16,
    _number_of_linenumbers: u16,
    characteristics: u32,
}

const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// `(rva, length)` of the module's first executable section — the range worth
/// searching for a code signature.
///
/// Bounding the scan to executable bytes is not just a speed matter: a pattern
/// that also appears in `.data` or `.rdata` would make [`crate::scan`]'s
/// uniqueness check fail over a match that could never have been executed.
///
/// Safety: `base` must point at a fully-mapped, valid PE image.
pub unsafe fn code_range(base: *mut u8) -> Option<(usize, usize)> {
    unsafe {
        let nt = nt_headers(base);
        if (*nt).signature != 0x0000_4550 {
            return None;
        }
        // Section headers follow the optional header, whose size the file
        // header states rather than the format fixing it.
        let optional_size = (*nt).file_header.size_of_optional_header as usize;
        let first = (nt as *const u8)
            .add(std::mem::size_of::<u32>() + std::mem::size_of::<ImageFileHeader>() + optional_size)
            as *const ImageSectionHeader;
        for i in 0..(*nt).file_header.number_of_sections as usize {
            let section = &*first.add(i);
            if section.characteristics & IMAGE_SCN_MEM_EXECUTE != 0 && section.virtual_size > 0 {
                return Some((section.virtual_address as usize, section.virtual_size as usize));
            }
        }
        None
    }
}
