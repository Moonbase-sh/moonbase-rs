use windows_sys::Win32::System::SystemInformation::{
    ComputerNamePhysicalDnsHostname, GetComputerNameExW, GetSystemFirmwareTable,
};

/// 'RSMB', the raw SMBIOS firmware table provider, unreversed. MSDN's "reverse the
/// characters" note is about the table id parameter, not the provider signature;
/// reversing this yields 0x424D5352, which no provider matches.
const RSMB: u32 = 0x5253_4D42;

/// Size of the `RawSMBIOSData` header `GetSystemFirmwareTable` puts before the
/// structure table: `Used20CallingMethod`, three version bytes, a `DWORD` length.
const RAW_SMBIOS_HEADER: usize = 8;

/// The SMBIOS structure table, without its `RawSMBIOSData` header. Empty when the
/// firmware table cannot be read.
pub(super) fn read_smbios_table() -> Vec<u8> {
    // SAFETY: the first call only sizes the table; the second writes at most
    // `size` bytes into a buffer of exactly that size.
    let raw = unsafe {
        let size = GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0);
        if size == 0 {
            return Vec::new();
        }
        let mut raw = vec![0u8; size as usize];
        let written = GetSystemFirmwareTable(RSMB, 0, raw.as_mut_ptr().cast(), size);
        if written == 0 || written > size {
            return Vec::new();
        }
        raw.truncate(written as usize);
        raw
    };

    if raw.len() <= RAW_SMBIOS_HEADER {
        return Vec::new();
    }
    let declared = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let available = raw.len() - RAW_SMBIOS_HEADER;
    raw[RAW_SMBIOS_HEADER..RAW_SMBIOS_HEADER + declared.min(available)].to_vec()
}

/// The physical DNS host name through the wide API, so a name with non-ASCII
/// letters arrives intact rather than through the ANSI code page.
pub(super) fn read_host_name() -> String {
    let mut buffer = [0u16; 256];
    let mut size = buffer.len() as u32;
    // SAFETY: `size` tells the call how many UTF-16 units `buffer` holds, and it
    // writes back how many it used.
    let ok = unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, buffer.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(size as usize).min(buffer.len())])
}
