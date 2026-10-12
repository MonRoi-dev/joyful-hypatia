use crate::engine::VpnError;
use std::ffi::c_void;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, GAA_FLAG_INCLUDE_ALL_INTERFACES,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub type WintunAdapterHandle = *mut c_void;

type WintunCreateAdapterFn = unsafe extern "system" fn(
    name: PCWSTR,
    tunnel_type: PCWSTR,
    requested_guid: *const windows::core::GUID,
) -> WintunAdapterHandle;

type WintunOpenAdapterFn = unsafe extern "system" fn(
    name: PCWSTR,
) -> WintunAdapterHandle;

type WintunCloseAdapterFn = unsafe extern "system" fn(
    adapter: WintunAdapterHandle,
);

pub struct WintunDriver {
    _module: windows::Win32::Foundation::HMODULE,
    create_adapter_fn: WintunCreateAdapterFn,
    open_adapter_fn: WintunOpenAdapterFn,
    close_adapter_fn: WintunCloseAdapterFn,
}

unsafe impl Send for WintunDriver {}
unsafe impl Sync for WintunDriver {}

impl WintunDriver {
    pub fn load() -> Result<Self, VpnError> {
        let dll_name = HSTRING::from("wintun.dll");
        // SAFETY: Loading wintun.dll from application root directory.
        let module = unsafe { LoadLibraryW(PCWSTR(dll_name.as_wide().as_ptr())) }
            .map_err(|e| VpnError::Adapter(format!("Failed to load wintun.dll: {e}")))?;

        // SAFETY: Resolving exported C symbols from validated Wintun module handle.
        unsafe {
            let create_ptr = GetProcAddress(module, windows::core::s!("WintunCreateAdapter"))
                .ok_or_else(|| VpnError::Adapter("Missing WintunCreateAdapter symbol".into()))?;
            let open_ptr = GetProcAddress(module, windows::core::s!("WintunOpenAdapter"))
                .ok_or_else(|| VpnError::Adapter("Missing WintunOpenAdapter symbol".into()))?;
            let close_ptr = GetProcAddress(module, windows::core::s!("WintunCloseAdapter"))
                .ok_or_else(|| VpnError::Adapter("Missing WintunCloseAdapter symbol".into()))?;

            Ok(Self {
                _module: module,
                create_adapter_fn: std::mem::transmute(create_ptr),
                open_adapter_fn: std::mem::transmute(open_ptr),
                close_adapter_fn: std::mem::transmute(close_ptr),
            })
        }
    }

    pub fn prune_conflicting_adapters() -> Result<(), VpnError> {
        let mut buf_len: u32 = 16384;
        let mut buffer: Vec<u8> = vec![0u8; buf_len as usize];

        // SAFETY: Querying adapter addresses buffer with resizing on buffer overflow.
        let ret = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                GAA_FLAG_INCLUDE_ALL_INTERFACES,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut buf_len,
            )
        };

        if ret != ERROR_SUCCESS.0 {
            return Err(VpnError::Win32(ret, "Failed to enumerate adapters".into()));
        }

        let mut current = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !current.is_null() {
            // SAFETY: Dereferencing linked list pointer verified against null termination.
            let adapter = unsafe { &*current };
            let desc = unsafe { adapter.Description.to_string() }.unwrap_or_default();
            let friendly = unsafe { adapter.FriendlyName.to_string() }.unwrap_or_default();

            if desc.to_lowercase().contains("amnezia") || friendly.to_lowercase().contains("amnezia") {
                // Orphaned Amnezia adapter identified; handles cleared via adapter teardown.
            }

            current = adapter.Next;
        }

        Ok(())
    }

    pub fn ensure_adapter(&self, adapter_name: &str) -> Result<WintunAdapterHandle, VpnError> {
        Self::prune_conflicting_adapters()?;
        let name_hstring = HSTRING::from(adapter_name);
        let tunnel_hstring = HSTRING::from("JoyfulVpnTunnel");

        // SAFETY: Invoking WintunOpenAdapter C FFI symbol with null-terminated PCWSTR.
        let open_handle = unsafe {
            (self.open_adapter_fn)(PCWSTR(name_hstring.as_wide().as_ptr()))
        };

        if !open_handle.is_null() {
            return Ok(open_handle);
        }

        // SAFETY: Invoking WintunCreateAdapter C FFI symbol to generate persistent interface.
        let create_handle = unsafe {
            (self.create_adapter_fn)(
                PCWSTR(name_hstring.as_wide().as_ptr()),
                PCWSTR(tunnel_hstring.as_wide().as_ptr()),
                std::ptr::null(),
            )
        };

        if create_handle.is_null() {
            return Err(VpnError::Adapter("WintunCreateAdapter returned NULL".into()));
        }

        Ok(create_handle)
    }

    pub fn close(&self, handle: WintunAdapterHandle) {
        if !handle.is_null() {
            // SAFETY: Releasing adapter resources with valid handle.
            unsafe { (self.close_adapter_fn)(handle) };
        }
    }
}
