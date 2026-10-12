use std::sync::Arc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, RegisterServiceCtrlHandlerExW, SetServiceStatus,
    StartServiceCtrlDispatcherW, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_INTERROGATE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR_NORMAL, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};

pub const SERVICE_NAME: PCWSTR = w!("JoyfulVpnCore");
pub const SERVICE_DISPLAY_NAME: PCWSTR = w!("Joyful VPN Background Core Service");

static mut STATUS_HANDLE: Option<SERVICE_STATUS_HANDLE> = None;
static mut CURRENT_STATUS: SERVICE_STATUS = SERVICE_STATUS {
    dwServiceType: SERVICE_WIN32_OWN_PROCESS,
    dwCurrentState: SERVICE_STOPPED,
    dwControlsAccepted: 0,
    dwWin32ExitCode: 0,
    dwServiceSpecificExitCode: 0,
    dwCheckPoint: 0,
    dwWaitHint: 0,
};

pub struct ServiceManager;

impl ServiceManager {
    pub fn install_service() -> Result<(), String> {
        let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_str = current_exe.to_str().ok_or("Invalid executable path")?;
        let bin_path = format!("\"{}\" --service-scm", exe_str);
        let wide_bin_path: Vec<u16> = bin_path.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            let scm = match OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS) {
                Ok(h) => h,
                Err(e) => return Err(format!("Failed to open SCM: {e}")),
            };

            let svc = match CreateServiceW(
                scm,
                SERVICE_NAME,
                SERVICE_DISPLAY_NAME,
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                PCWSTR(wide_bin_path.as_ptr()),
                None,
                None,
                None,
                None,
                None,
            ) {
                Ok(s) => s,
                Err(e) => {
                    let _ = CloseServiceHandle(scm);
                    return Err(format!("CreateServiceW failed: {e}"));
                }
            };

            let desc = w!("Manages Wintun tunnels, routing tables, and local DNS proxy for Joyful VPN.");
            let mut desc_struct = SERVICE_DESCRIPTIONW {
                lpDescription: windows::core::PWSTR(desc.as_ptr() as *mut u16),
            };

            let _ = ChangeServiceConfig2W(
                svc,
                SERVICE_CONFIG_DESCRIPTION,
                Some(&mut desc_struct as *mut _ as *const _),
            );

            let _ = CloseServiceHandle(svc);
            let _ = CloseServiceHandle(scm);
        }

        Ok(())
    }

    pub fn uninstall_service() -> Result<(), String> {
        unsafe {
            let scm = match OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS) {
                Ok(h) => h,
                Err(e) => return Err(format!("Failed to open SCM: {e}")),
            };

            let svc = match OpenServiceW(scm, SERVICE_NAME, SERVICE_ALL_ACCESS) {
                Ok(s) => s,
                Err(e) => {
                    let _ = CloseServiceHandle(scm);
                    return Err(format!("Service JoyfulVpnCore not found: {e}"));
                }
            };

            let del_res = DeleteService(svc);
            let _ = CloseServiceHandle(svc);
            let _ = CloseServiceHandle(scm);

            if let Err(e) = del_res {
                return Err(format!("Failed to delete service: {e}"));
            }
        }

        Ok(())
    }

    pub fn run_service_dispatcher() -> Result<(), String> {
        let table = [
            SERVICE_TABLE_ENTRYW {
                lpServiceName: windows::core::PWSTR(SERVICE_NAME.as_ptr() as *mut u16),
                lpServiceProc: Some(service_main),
            },
            SERVICE_TABLE_ENTRYW {
                lpServiceName: windows::core::PWSTR(std::ptr::null_mut()),
                lpServiceProc: None,
            },
        ];

        unsafe {
            if let Err(e) = StartServiceCtrlDispatcherW(table.as_ptr()) {
                return Err(format!("StartServiceCtrlDispatcherW failed: {e}"));
            }
        }

        Ok(())
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows::core::PWSTR) {
    let handle = match RegisterServiceCtrlHandlerExW(
        SERVICE_NAME,
        Some(service_ctrl_handler),
        None,
    ) {
        Ok(h) => h,
        Err(_) => return,
    };

    STATUS_HANDLE = Some(handle);
    CURRENT_STATUS.dwCurrentState = SERVICE_START_PENDING;
    CURRENT_STATUS.dwControlsAccepted = 0;
    let _ = SetServiceStatus(handle, &CURRENT_STATUS);

    CURRENT_STATUS.dwCurrentState = SERVICE_RUNNING;
    CURRENT_STATUS.dwControlsAccepted = SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN | SERVICE_CONTROL_INTERROGATE;
    let _ = SetServiceStatus(handle, &CURRENT_STATUS);

    let rt = tokio::runtime::Runtime::new().expect("Failed to initialize daemon runtime");
    rt.block_on(async {
        let daemon = Arc::new(crate::daemon::CoreDaemon::new());
        let _ = daemon.run_ipc_server().await;
    });

    CURRENT_STATUS.dwCurrentState = SERVICE_STOPPED;
    let _ = SetServiceStatus(handle, &CURRENT_STATUS);
}

unsafe extern "system" fn service_ctrl_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            if let Some(h) = STATUS_HANDLE {
                CURRENT_STATUS.dwCurrentState = SERVICE_STOP_PENDING;
                let _ = SetServiceStatus(h, &CURRENT_STATUS);
                std::process::exit(0);
            }
            ERROR_SUCCESS.0
        }
        SERVICE_CONTROL_INTERROGATE => {
            if let Some(h) = STATUS_HANDLE {
                let _ = SetServiceStatus(h, &CURRENT_STATUS);
            }
            ERROR_SUCCESS.0
        }
        _ => ERROR_SUCCESS.0,
    }
}
